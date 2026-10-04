//! `--shots` scenarios.
//!
//! The framework captures the live window; this module names what the
//! Settings capture scenarios show and builds the deterministic fixtures
//! they stage. A scenario never reads the host: hardware panes render
//! fixture data so a capture is the same on every machine.
//!
//! - no scenario: the Appearance pane, wide, so the selected sidebar row and
//!   a populated content pane are both visible;
//! - a category slug (`network`, `sound`, `region_formats`, ...): that pane,
//!   with the hardware panes showing fixture data;
//! - `<slug>-narrow`: that pane's content screen in a phone-sized window;
//! - `nav-narrow`: the narrow category list, the screen a collapse returns
//!   to;
//! - `network-wifi`, `sound-apps`, `accounts-populated`, `accounts-form`,
//!   `speakers-layout`: a populated Wi-Fi scan, per-application audio routing,
//!   the external accounts inventory with and without its add form, and the
//!   room layout step of setting up speakers.

use ui::widget::Widget;

use crate::categories::Category;

/// The viewport of a `-narrow` scenario: phone-sized.
pub const NARROW_SHOTS_VIEWPORT: (u32, u32) = (440, 820);

/// Every scenario name, for `--shots --scenario NAME`.
pub const SCENARIOS: &[&str] = &[
    "network",
    "bluetooth",
    "devices_sync",
    "sound",
    "speakers",
    "displays",
    "mouse_keyboard",
    "keyboard_shortcuts",
    "time_date",
    "users",
    "printers",
    "power",
    "appearance",
    "accessibility",
    "notifications",
    "default-apps",
    "storage",
    "external_accounts",
    "agent_models",
    "region_formats",
    "permissions",
    "updates",
    "about",
    "network-narrow",
    "appearance-narrow",
    "region_formats-narrow",
    "external_accounts-narrow",
    "power-narrow",
    "nav-narrow",
    "network-wifi",
    "sound-apps",
    "accounts-populated",
    "accounts-form",
    "speakers-layout",
];

/// A fixture a scenario puts in place of the live pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Demo {
    /// A Wi-Fi scan with one network's password prompt open.
    NetworkWifi,
    /// Two output devices and per-application routing.
    SoundApps,
    /// The accounts inventory, populated.
    AccountsPopulated,
    /// The accounts inventory with the add form open.
    AccountsForm,
    /// Placing the speakers of a new set around the room.
    SpeakersLayout,
}

/// What a named scenario asks the app to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scenario {
    /// The selected category.
    pub category: Category,
    /// A narrow window shows the pane full screen.
    pub narrow_content: bool,
    /// A narrow window shows the category list.
    pub narrow_list: bool,
    /// A fixture to show instead of the live pane.
    pub demo: Option<Demo>,
}

impl Scenario {
    /// The scenario named `name` (`None`: a plain `--shots`). A `-narrow`
    /// name is its base scenario in a phone-sized window.
    pub fn parse(name: Option<&str>) -> Self {
        let plain = Self {
            category: Category::Appearance,
            narrow_content: false,
            narrow_list: false,
            demo: None,
        };
        let Some(name) = name else {
            return plain;
        };
        if name == "nav-narrow" {
            return Self {
                category: Category::RegionFormats,
                narrow_list: true,
                ..plain
            };
        }
        if let Some(base) = name.strip_suffix("-narrow") {
            return Self {
                narrow_content: true,
                ..Self::parse(Some(base))
            };
        }
        match name {
            "network" | "network-wifi" => Self {
                category: Category::Network,
                demo: Some(Demo::NetworkWifi),
                ..plain
            },
            "sound" | "sound-apps" => Self {
                category: Category::Sound,
                demo: Some(Demo::SoundApps),
                ..plain
            },
            "external_accounts" | "accounts-populated" => Self {
                category: Category::ExternalAccounts,
                demo: Some(Demo::AccountsPopulated),
                ..plain
            },
            "speakers-layout" => Self {
                category: Category::Speakers,
                demo: Some(Demo::SpeakersLayout),
                ..plain
            },
            "accounts-form" => Self {
                category: Category::ExternalAccounts,
                demo: Some(Demo::AccountsForm),
                ..plain
            },
            slug => Self {
                category: Category::from_slug(slug).unwrap_or(plain.category),
                ..plain
            },
        }
    }
}

/// The viewport a scenario is captured at, when it is not the default.
pub fn scenario_viewport(name: &str) -> Option<(u32, u32)> {
    name.ends_with("-narrow").then_some(NARROW_SHOTS_VIEWPORT)
}

/// The `PipeWire` graph of the capture machine: two output devices, the
/// music app routed to the headphones while the browser follows the
/// speakers, and a meeting app on the microphone. The sound pane and the
/// published resources both read it.
pub(crate) fn audio_graph() -> media::audio::AudioGraph {
    use media::audio::{AudioDevice as GDevice, AudioGraph, AudioStream};

    let sink = |id, serial, name: &str, def| GDevice {
        id,
        serial,
        name: name.to_string(),
        description: name.to_string(),
        is_default: def,
        volume: Some(0.7),
        muted: Some(false),
        channels: 2,
        codec: None,
    };
    let stream = |id, serial, app: &str, media: &str, target, playback, vol, mute| AudioStream {
        id,
        serial,
        app: app.to_string(),
        media_name: Some(media.to_string()),
        playback,
        target_id: Some(target),
        volume: Some(vol),
        muted: Some(mute),
    };
    AudioGraph {
        sinks: vec![
            sink(51, 51, "Speakers", true),
            sink(91, 269, "Headphones", false),
        ],
        sources: vec![sink(52, 52, "Built-in Microphone", true)],
        playback_streams: vec![
            // Routed away from the default onto the Headphones.
            stream(84, 300, "Music", "Nightcall", 91, true, 0.85, false),
            // Following the default (Speakers).
            stream(90, 320, "Browser", "Conference call", 51, true, 0.60, false),
        ],
        record_streams: vec![stream(
            95,
            340,
            "Meeting",
            "Microphone",
            52,
            false,
            0.75,
            false,
        )],
        hardware: vec![],
    }
}

/// The outputs the compositor publishes on the capture machine: the laptop's
/// own panel and a monitor on a `DisplayPort` connector, side by side.
pub(crate) fn display_snapshot() -> settings_store::hardware::DisplayHardwareSnapshot {
    use settings_store::hardware::{
        DisplayHardwareSnapshot, DisplayOutputSnapshot, LiveOutputPlacement, MonitorIdentity,
        OutputDescriptor, OutputMode,
    };

    let mode = |width, height, refresh_mhz| OutputMode {
        width,
        height,
        refresh_mhz,
    };
    let output = |connector: &str,
                  builtin: bool,
                  modes: Vec<OutputMode>,
                  monitor: Option<MonitorIdentity>| DisplayOutputSnapshot {
        descriptor: OutputDescriptor {
            connector: connector.into(),
            connector_label: connector.into(),
            modes: modes.clone(),
            hdr: false,
            vrr: false,
            color_profiles: Vec::new(),
            monitor,
        },
        current_mode: modes
            .first()
            .copied()
            .unwrap_or_else(|| mode(1920, 1080, 60_000)),
        scale: 1.0,
        builtin,
        sink_hdr: false,
        hdr_unavailable_reason: None,
        sink_vrr: false,
        vrr_unavailable_reason: None,
        color_profile_labels: std::collections::BTreeMap::new(),
    };
    DisplayHardwareSnapshot {
        generated_unix_ms: 0,
        backend: "capture".into(),
        publisher_pid: 0,
        publisher_start_ticks: 0,
        outputs: vec![
            output(
                "eDP-1",
                true,
                vec![
                    mode(1920, 1080, 60_000),
                    mode(1600, 900, 60_000),
                    mode(1280, 720, 60_000),
                ],
                None,
            ),
            output(
                "DP-2",
                false,
                vec![
                    mode(2560, 1440, 59_950),
                    mode(1920, 1080, 60_000),
                    mode(1680, 1050, 59_950),
                ],
                Some(MonitorIdentity {
                    make: "Dell".into(),
                    model: "DELL U2720Q".into(),
                    serial: "7XKQ3H2".into(),
                    physical_size_mm: Some((597, 336)),
                }),
            ),
        ],
        layout: vec![
            LiveOutputPlacement {
                connector: "eDP-1".into(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            LiveOutputPlacement {
                connector: "DP-2".into(),
                x: 1920,
                y: 0,
                width: 2560,
                height: 1440,
            },
        ],
    }
}

/// The battery's charge-limit control on the capture machine.
pub(crate) fn charge_capability() -> os_actions::ChargeCapCapability {
    os_actions::ChargeCapCapability {
        backend: os_actions::ChargeCapBackend::PowerSupply,
        current: 80,
        choices: vec![60, 70, 80, 90, 100],
        supports_start_threshold: false,
    }
}

/// The accounts of `directory`, as the shared store holds them, for the
/// resources published beside the pane that lists them.
pub(crate) fn account_inventory(
    directory: &crate::accounts::AccountDirectory,
) -> Vec<accounts::Account> {
    crate::accounts::AccountKind::ALL
        .into_iter()
        .flat_map(|kind| directory.views_of(kind))
        .map(|view| {
            let mut account =
                accounts::Account::new(accounts::AccountId::from(view.id), view.kind.type_id());
            account.display_name = view.label;
            account
        })
        .collect()
}

/// A `NetworkPane` populated with a deterministic scan list, a connected
/// wired link, and a secure network's password prompt pre-expanded — the
/// state the connect flow reaches after clicking "Connect" on a locked
/// network. Used by the `netdemo` capture.
pub(crate) fn network_demo_pane() -> Box<dyn Widget> {
    use os_actions::{WifiNetworkInfo, WiredInfo};
    let nets = vec![
        WifiNetworkInfo {
            ssid: "Home".into(),
            signal: 4,
            secure: true,
            security: os_actions::WifiSecurity::Wpa3,
            connected: true,
        },
        WifiNetworkInfo {
            ssid: "Cafe Guest".into(),
            signal: 3,
            secure: false,
            security: os_actions::WifiSecurity::Open,
            connected: false,
        },
        WifiNetworkInfo {
            ssid: "Office 5G".into(),
            signal: 2,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected: false,
        },
    ];
    let pane = crate::panes::network::NetworkPane::new_demo(nets)
        .with_wired(WiredInfo {
            connected: true,
            device: Some("enp3s0".into()),
            speed: Some("1000 Mb/s".into()),
            ipv4: Some("192.168.1.42".into()),
        })
        .with_expanded("Office 5G");
    Box::new(pane)
}

/// An `AudioPane` seeded with a deterministic `PipeWire` graph: two output
/// devices (Speakers + Headphones), and per-application streams — the music app
/// routed to the Headphones while the browser stays on the Speakers, plus a
/// meeting app on the microphone — so the still proves the per-application
/// routing surface (a picker, volume, and mute per app). Sample data lives only
/// behind `--shots`; the live pane enumerates the real graph.
pub(crate) fn audio_demo_pane() -> Box<dyn Widget> {
    use crate::panes::audio::{AudioDevice, AudioDevices, AudioPane, RealDevice, RealInput};
    use media::audio::NoopGraphControl;
    use std::sync::Arc;

    let graph = audio_graph();
    // The device pickers read the local device view; give it the same two sinks
    // so the output-device chooser is shown alongside the per-app rows.
    let devices = AudioDevices {
        sinks: vec![
            AudioDevice {
                id: 51,
                name: "Speakers".into(),
                default: true,
            },
            AudioDevice {
                id: 91,
                name: "Headphones".into(),
                default: false,
            },
        ],
        sources: vec![AudioDevice {
            id: 52,
            name: "Built-in Microphone".into(),
            default: true,
        }],
    };
    let disp = crate::dispatcher::dispatcher_or_real(None);
    let pane = AudioPane::with_controls(disp, Arc::new(RealInput), 0.65, 0.7)
        .with_devices(devices, Arc::new(RealDevice))
        .with_graph(graph, Arc::new(NoopGraphControl));
    Box::new(pane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scenario_name_resolves_to_its_pane() {
        assert_eq!(Scenario::parse(None).category, Category::Appearance);
        for name in SCENARIOS {
            let scenario = Scenario::parse(Some(name));
            let slug = name.strip_suffix("-narrow").unwrap_or(name);
            if let Some(category) = Category::from_slug(slug) {
                assert_eq!(scenario.category, category, "{name}");
            }
            assert_eq!(
                scenario_viewport(name).is_some(),
                name.ends_with("-narrow"),
                "{name}"
            );
        }
        let list = Scenario::parse(Some("nav-narrow"));
        assert!(list.narrow_list && !list.narrow_content);
        assert!(Scenario::parse(Some("region_formats-narrow")).narrow_content);
    }

    #[test]
    fn every_category_has_a_scenario() {
        for category in Category::ALL {
            assert!(SCENARIOS.contains(&category.slug()), "{category:?}");
        }
    }
}
