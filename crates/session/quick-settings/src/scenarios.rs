//! The panel's screenshot scenarios: a laptop with Wi-Fi, Bluetooth, a VPN
//! profile, a battery and the person's devices, and each page and section
//! open on it. The readings are stand-ins ([`readings`]); the page or
//! section showing is staged through the panel's own actions ([`actions`]).

use ui::icons::Icon;
use ui::prelude::Invoke;

use connectivity::model::{
    Accessory, Bluetooth, Link, LinkKind, Nearby, Network, SavedNetwork, Vpn, VpnProfile, Wifi,
    WifiNetwork,
};

use crate::hosted::{HostedActions, HostedRow};
use crate::resource::QuickSettings;
use crate::{
    HandoffTarget, PowerProfile, QsSection, QuickSettingsState, SoundOutput, WifiPasswordPrompt,
};

pub const NAMES: &[&str] = &[
    "controls",
    "wifi",
    "wifi-password",
    "wifi-hidden",
    "bluetooth",
    "vpn",
    "battery",
    "power-mode",
    "sound",
    "devices",
    "clipboard",
    "paste",
    "accessory",
    "edit",
    "short",
    "now-playing",
    "silent",
];

/// The actions that bring the panel to `name`.
pub fn actions(name: Option<&str>) -> Vec<Invoke<QuickSettings>> {
    let open = |section| vec![QuickSettings::OPEN_SECTION.with(section)];
    match name {
        Some("wifi" | "wifi-password" | "wifi-hidden") => open(QsSection::Wifi),
        Some("bluetooth") => open(QsSection::Bluetooth),
        Some("vpn") => open(QsSection::Vpn),
        Some("battery") => open(QsSection::Battery),
        Some("power-mode") => open(QsSection::PowerMode),
        Some("sound") => open(QsSection::Sound),
        Some("devices") => open(QsSection::Devices),
        Some("clipboard") => open(QsSection::Clipboard),
        Some("paste") => vec![QuickSettings::PASTE.invoke()],
        Some("accessory") => vec![QuickSettings::SHOW_ACCESSORY.with(HEADPHONES.to_owned())],
        Some("edit") => vec![QuickSettings::EDIT.with(true)],
        _ => Vec::new(),
    }
}

const HEADPHONES: &str = "00:1B:66:A1:B2:C3";

/// What the device reads in scenario `name`.
#[allow(
    clippy::too_many_lines,
    reason = "one stand-in device, written out field by field"
)]
pub fn readings(name: Option<&str>) -> QuickSettingsState {
    let mut state = QuickSettingsState::fixture();
    state.brightness = 0.7;
    state.network = Network {
        wifi: Wifi {
            available: true,
            on: true,
            hardware_blocked: false,
            networks: vec![
                network("Home", 4, true, true),
                network("Office", 3, false, true),
                network("Cafe guest", 2, false, false),
                WifiNetwork {
                    secure: false,
                    ..network("Library", 1, false, false)
                },
            ],
        },
        saved: vec![
            SavedNetwork {
                name: "Home".into(),
                id: "home".into(),
                autoconnect: true,
                ..SavedNetwork::default()
            },
            SavedNetwork {
                name: "Grandma's".into(),
                id: "grandma".into(),
                autoconnect: false,
                ..SavedNetwork::default()
            },
        ],
        link: Some(Link {
            kind: LinkKind::Wifi,
            name: Some("Home".into()),
            signal: 4,
        }),
        ..Network::default()
    };
    state.status.link = state.network.link.clone();
    state.status.battery = Some(tilo_power_mgmt::BatterySnapshot {
        present: true,
        percent: 76,
        state: tilo_power_mgmt::BatteryState::Discharging,
    });
    state.status.battery_time = Some(tilo_power_mgmt::BatteryTime::ToEmpty(214));
    state.bluetooth = Bluetooth {
        available: true,
        on: true,
        accessories: vec![
            Accessory {
                name: "Studio headphones".into(),
                address: HEADPHONES.into(),
                battery: Some(80),
                connected: true,
            },
            Accessory {
                name: "Keyboard".into(),
                address: "00:1B:66:D4:E5:F6".into(),
                battery: None,
                connected: false,
            },
        ],
        nearby: vec![Nearby {
            name: "Kitchen speaker".into(),
            address: "00:1B:66:77:88:99".into(),
            signal: Some(-62),
        }],
        ..Bluetooth::default()
    };
    state.vpn = Vpn {
        profiles: vec![VpnProfile {
            name: "Work".into(),
            id: "7c1e3a5e-0d61-4f3c-9b1a-2d6f9f0e1a11".into(),
            kind: "wireguard".into(),
            connected: false,
        }],
        ..Vpn::default()
    };
    state.power_profiles = vec![
        PowerProfile::Saver,
        PowerProfile::Balanced,
        PowerProfile::Performance,
    ];
    state.awake_holders = vec!["Video Player".into()];
    state.personal_devices = Some(2);
    // A session with no audio server, as a build container runs one: the
    // sound service reads no graph, and lands nothing in the panel.
    if name == Some("silent") {
        crate::sound::land(&mut state, &audio::sound::Sound::default());
        return state;
    }
    state.sound_outputs = vec![
        SoundOutput {
            node: 41,
            name: "Speakers".into(),
            current: true,
        },
        SoundOutput {
            node: 52,
            name: "Studio headphones".into(),
            current: false,
        },
    ];
    state.device_rows = vec![
        HostedRow {
            key: "phone".into(),
            title: "Phone".into(),
            detail: "Next to you".into(),
            icon: Icon::Phone,
            selected: false,
            enabled: true,
            actions: HostedActions::Control {
                name: "Control with this pointer".into(),
                controlling: false,
                controllable: true,
            },
        },
        HostedRow {
            key: "speaker-request".into(),
            title: "Kitchen speaker".into(),
            detail: "Asks to play this computer's audio".into(),
            icon: Icon::Speaker,
            selected: false,
            enabled: true,
            actions: HostedActions::Pair {
                first: (Icon::Check, "Allow".into()),
                second: (Icon::X, "Decline".into()),
            },
        },
    ];
    let copies = [
        (
            "a1",
            "Meeting moved to 3 pm",
            "Just now, from this computer",
            true,
        ),
        (
            "b2",
            "https://tilo.so/docs/apps",
            "5 min ago, from Phone",
            false,
        ),
        (
            "c3",
            "Grocery list: oats, lemons, coffee",
            "1 h ago, from this computer",
            false,
        ),
    ];
    state.clipboard = crate::ClipboardView {
        total: copies.len(),
        rows: copies
            .iter()
            .map(|(key, title, detail, pinned)| HostedRow {
                key: (*key).into(),
                title: (*title).into(),
                detail: (*detail).into(),
                icon: Icon::FileText,
                selected: false,
                enabled: true,
                actions: HostedActions::Clipboard {
                    pinned: *pinned,
                    pin: String::new(),
                    remove: String::new(),
                },
            })
            .collect(),
    };
    state.notifications = 3;
    state.handoff_targets = Some(vec![
        HandoffTarget {
            id: "laptop".into(),
            label: "This laptop".into(),
            online: true,
            local: true,
        },
        HandoffTarget {
            id: "desk".into(),
            label: "Desk computer".into(),
            online: true,
            local: false,
        },
        HandoffTarget {
            id: "tablet".into(),
            label: "Tablet".into(),
            online: false,
            local: false,
        },
    ]);
    match name {
        Some("wifi-password") => {
            state.wifi_password = Some(WifiPasswordPrompt {
                error: Some("That password didn't work".into()),
                ..WifiPasswordPrompt::new("Cafe guest")
            });
        }
        Some("wifi-hidden") => {
            // The form under the networks: a short list keeps it in view.
            state.network.wifi.networks.truncate(2);
            state.wifi_hidden = Some(crate::HiddenNetworkPrompt::new());
        }
        Some("now-playing") => state.now_playing = Some(crate::NowPlaying::fixture()),
        Some("short") => {
            state.displays = vec![
                crate::DisplayBrightness {
                    connector: "eDP-1".into(),
                    name: "Built-in display".into(),
                    level: 0.7,
                },
                crate::DisplayBrightness {
                    connector: "DP-2".into(),
                    name: "Dell U2720Q".into(),
                    level: 0.5,
                },
            ];
        }
        _ => {}
    }
    state
}

fn network(name: &str, signal: u8, connected: bool, known: bool) -> WifiNetwork {
    WifiNetwork {
        name: name.into(),
        signal,
        secure: true,
        connected,
        known,
    }
}
