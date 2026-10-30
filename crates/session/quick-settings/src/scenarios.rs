//! The panel's screenshot scenarios: a laptop with Wi-Fi, Bluetooth, a VPN
//! profile, a battery and the person's devices, and each page and section
//! open on it. The readings are stand-ins ([`readings`]); the page or
//! section showing is staged through the panel's own actions ([`actions`]).

use ui::icons::Icon;
use ui::prelude::Invoke;

use crate::hosted::{HostedActions, HostedRow};
use crate::resource::QuickSettings;
use crate::{
    BtDevice, HandoffTarget, NotificationRow, NotificationsView, PowerProfile, QsSection,
    QuickSettingsState, SoundOutput, WifiNetwork, WifiPasswordPrompt,
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
    "notifications",
    "notification",
    "accessory",
    "edit",
    "short",
    "now-playing",
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
        Some("notifications") => vec![QuickSettings::SHOW_NOTIFICATIONS.invoke()],
        Some("notification") => vec![
            QuickSettings::SHOW_NOTIFICATIONS.invoke(),
            QuickSettings::READ_NOTIFICATION.with(3_u64),
        ],
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
    state.radios.wifi_available = true;
    state.radios.wifi_enabled = true;
    state.wifi = true;
    state.status.link = Some(os_actions::LinkStatus {
        kind: os_actions::LinkKind::Wifi,
        name: Some("Home".into()),
        signal: 4,
    });
    state.status.battery = Some(tilo_power_mgmt::BatterySnapshot {
        present: true,
        percent: 76,
        state: tilo_power_mgmt::BatteryState::Discharging,
    });
    state.status.battery_time = Some(tilo_power_mgmt::BatteryTime::ToEmpty(214));
    state.bluetooth_available = true;
    state.bluetooth = true;
    state.bt_devices = vec![
        BtDevice {
            name: "Studio headphones".into(),
            address: HEADPHONES.into(),
            battery: 80,
            connected: true,
        },
        BtDevice {
            name: "Keyboard".into(),
            address: "00:1B:66:D4:E5:F6".into(),
            battery: 0,
            connected: false,
        },
    ];
    state.wifi_networks = vec![
        network("Home", 4, true, true),
        network("Office", 3, false, true),
        network("Cafe guest", 2, false, false),
        WifiNetwork {
            secure: false,
            security: os_actions::WifiSecurity::Open,
            ..network("Library", 1, false, false)
        },
    ];
    state.saved_networks = vec![
        crate::SavedNetwork {
            name: "Home".into(),
            profile: "home".into(),
            autoconnect: true,
        },
        crate::SavedNetwork {
            name: "Grandma's".into(),
            profile: "grandma".into(),
            autoconnect: false,
        },
    ];
    state.vpns = vec![os_actions::VpnConnectionInfo {
        name: "Work".into(),
        uuid: "7c1e3a5e-0d61-4f3c-9b1a-2d6f9f0e1a11".into(),
        active: false,
        kind: "wireguard".into(),
    }];
    state.power_profiles = vec![
        PowerProfile::Saver,
        PowerProfile::Balanced,
        PowerProfile::Performance,
    ];
    state.awake_holders = vec!["Video Player".into()];
    state.personal_devices = Some(2);
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
        caption: String::new(),
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
    state.notifications = NotificationsView {
        rows: vec![
            notification(
                1,
                "chat",
                "Chat",
                "Ana",
                "Are we still on for lunch?",
                "2 min ago",
                true,
            ),
            notification(
                2,
                "chat",
                "Chat",
                "Sam",
                "Sent you the slides",
                "10 min ago",
                true,
            ),
            NotificationRow {
                actions: vec![
                    ("dismiss".into(), "Dismiss".into()),
                    ("snooze".into(), "Snooze".into()),
                ],
                ..notification(
                    3,
                    "calendar",
                    "Calendar",
                    "Design review",
                    "In 15 minutes in the large room. Bring the prototype and the notes from \
                     last week's session; we will go through the open questions one by one.",
                    "15 min ago",
                    false,
                )
            },
        ],
        quiet: None,
        can_undo: false,
    };
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
            state.wifi_networks.truncate(2);
            state.wifi_hidden = Some(crate::HiddenNetworkPrompt::new());
        }
        Some("notifications") => {
            state.do_not_disturb = true;
            state.notifications.quiet = Some("Do Not Disturb is on".into());
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

fn network(ssid: &str, signal: u8, connected: bool, known: bool) -> WifiNetwork {
    WifiNetwork {
        ssid: ssid.into(),
        signal,
        secure: true,
        security: os_actions::WifiSecurity::Wpa2,
        connected,
        known,
    }
}

fn notification(
    id: u64,
    group: &str,
    app: &str,
    title: &str,
    body: &str,
    age: &str,
    opens: bool,
) -> NotificationRow {
    NotificationRow {
        id,
        group: group.into(),
        app: app.into(),
        title: title.into(),
        body: body.into(),
        age: age.into(),
        actions: Vec::new(),
        agent: None,
        opens,
    }
}
