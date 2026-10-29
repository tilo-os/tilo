//! The panel on a device, headless: the device host reads a stand-in
//! machine on its workers and the readings reach the panel; the panel's
//! effects reach the machine's dispatcher, its players and the session.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufReader, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use media::audio::default_node::VolumeState;
use settings_store::hardware::QuickSettingsConfig;
use tilo_quick_settings::actions::QsActionAvailability;
use tilo_quick_settings::device::{
    Device, DeviceHost, DeviceReads, PlayerLink, Preferences, SessionRequest,
};
use tilo_quick_settings::host::Lent;
use tilo_quick_settings::{
    DisplayBrightness, PowerProfile, QsEffect, QsSection, QuickSettings, QuickSettingsApp,
    QuickSettingsState,
};
use ui::framework::testing::HeadlessApp;

/// The stand-in machine: what it reads, and every change made to it.
#[derive(Clone, Default)]
struct Machine(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    brightness: f32,
    volume: Option<VolumeState>,
    bluetooth: bool,
    power: Option<os_actions::PowerProfile>,
    radios: os_actions::RadioState,
    vpns: Vec<os_actions::VpnConnectionInfo>,
    networks: Vec<os_actions::WifiNetworkInfo>,
    accessories: Vec<os_actions::BtDeviceInfo>,
    battery: u8,
    /// Changes made, in order.
    calls: Vec<String>,
    /// Changes the machine refuses, by name.
    refuses: Vec<&'static str>,
}

impl Machine {
    fn laptop() -> Self {
        let machine = Self::default();
        {
            let mut inner = machine.0.lock().unwrap();
            inner.brightness = 0.7;
            inner.volume = Some(VolumeState {
                level: 0.3,
                muted: false,
            });
            inner.bluetooth = true;
            inner.power = Some(os_actions::PowerProfile::Performance);
            inner.radios = os_actions::RadioState {
                wifi_available: true,
                wifi_enabled: true,
                ..os_actions::RadioState::default()
            };
            inner.vpns = vec![os_actions::VpnConnectionInfo {
                name: "Work".into(),
                uuid: "work".into(),
                active: false,
                kind: "wireguard".into(),
            }];
            inner.networks = vec![os_actions::WifiNetworkInfo {
                ssid: "Home".into(),
                signal: 4,
                secure: true,
                security: os_actions::WifiSecurity::default(),
                connected: true,
                known: true,
            }];
            inner.accessories = vec![os_actions::BtDeviceInfo {
                name: "Headphones".into(),
                mac: "00:1B:66:A1:B2:C3".into(),
                battery: 80,
                connected: true,
            }];
            inner.battery = 64;
        }
        machine
    }

    fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    fn change(&self, call: String, name: &'static str) -> Result<(), String> {
        let mut inner = self.0.lock().unwrap();
        inner.calls.push(call);
        if inner.refuses.contains(&name) {
            Err(format!("{name} refused"))
        } else {
            Ok(())
        }
    }
}

impl DeviceReads for Machine {
    fn brightness(&self) -> Option<f32> {
        Some(self.0.lock().unwrap().brightness)
    }
    fn displays(&self) -> Vec<DisplayBrightness> {
        Vec::new()
    }
    fn output_device(&self) -> bool {
        true
    }
    fn volume(&self) -> Option<VolumeState> {
        self.0.lock().unwrap().volume
    }
    fn bluetooth(&self) -> Option<bool> {
        Some(self.0.lock().unwrap().bluetooth)
    }
    fn power(&self) -> Option<os_actions::PowerProfile> {
        self.0.lock().unwrap().power
    }
    fn power_profiles(&self) -> Option<Vec<os_actions::PowerProfile>> {
        Some(vec![
            os_actions::PowerProfile::Saver,
            os_actions::PowerProfile::Balanced,
            os_actions::PowerProfile::Performance,
        ])
    }
    fn wired(&self) -> Option<os_actions::WiredInfo> {
        None
    }
    fn battery(&self) -> tilo_power_mgmt::BatterySnapshot {
        tilo_power_mgmt::BatterySnapshot {
            present: true,
            percent: self.0.lock().unwrap().battery,
            state: tilo_power_mgmt::BatteryState::Discharging,
        }
    }
    fn battery_time(&self) -> Option<tilo_power_mgmt::BatteryTime> {
        None
    }
    fn radios(&self) -> Option<os_actions::RadioState> {
        Some(self.0.lock().unwrap().radios.clone())
    }
    fn privacy(&self) -> os_actions::PrivacyStatus {
        os_actions::PrivacyStatus::default()
    }
    fn vpns(&self) -> Option<Vec<os_actions::VpnConnectionInfo>> {
        Some(self.0.lock().unwrap().vpns.clone())
    }
    fn link(&self) -> Option<os_actions::LinkStatus> {
        Some(os_actions::LinkStatus {
            kind: os_actions::LinkKind::Wifi,
            name: Some("Home".into()),
            signal: 4,
        })
    }
    fn wifi_networks(&self) -> Result<Vec<os_actions::WifiNetworkInfo>, String> {
        Ok(self.0.lock().unwrap().networks.clone())
    }
    fn scan_wifi(&self) -> Result<(), String> {
        Ok(())
    }
    fn bt_devices(&self) -> Option<Vec<os_actions::BtDeviceInfo>> {
        Some(self.0.lock().unwrap().accessories.clone())
    }
    fn firmware_updates(&self) -> Option<usize> {
        Some(2)
    }
    fn availability(&self) -> QsActionAvailability {
        QsActionAvailability::all()
    }
}

impl os_actions::Dispatcher for Machine {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        self.change(format!("brightness:{pct}"), "brightness")?;
        self.0.lock().unwrap().brightness = pct / 100.0;
        Ok(())
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        self.change(format!("volume:{pct}"), "volume")?;
        let mut inner = self.0.lock().unwrap();
        let muted = inner.volume.is_some_and(|volume| volume.muted);
        inner.volume = Some(VolumeState {
            level: pct / 100.0,
            muted,
        });
        drop(inner);
        Ok(())
    }
    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.change(format!("mute:{muted}"), "mute")?;
        let mut inner = self.0.lock().unwrap();
        if let Some(volume) = inner.volume.as_mut() {
            volume.muted = muted;
        }
        drop(inner);
        Ok(())
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.change(format!("wifi:{enabled}"), "wifi")?;
        self.0.lock().unwrap().radios.wifi_enabled = enabled;
        Ok(())
    }
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        self.change(format!("bluetooth:{enabled}"), "bluetooth")?;
        self.0.lock().unwrap().bluetooth = enabled;
        Ok(())
    }
    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.change(format!("disconnect:{address}"), "disconnect")?;
        for accessory in &mut self.0.lock().unwrap().accessories {
            if accessory.mac == address {
                accessory.connected = false;
            }
        }
        Ok(())
    }
    fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
        self.change(format!("vpn:{name}:{enabled}"), "vpn")
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        self.change(
            format!("join:{ssid}:{}", password.unwrap_or("")),
            "connect_wifi",
        )
    }
    fn set_power_profile(&self, profile: os_actions::PowerProfile) -> Result<(), String> {
        self.change(format!("power:{profile:?}"), "power")?;
        self.0.lock().unwrap().power = Some(profile);
        Ok(())
    }
}

/// The person's settings, in memory.
#[derive(Clone, Default)]
struct Kept(Arc<Mutex<Option<QuickSettingsConfig>>>);

impl Preferences for Kept {
    fn arrangement(&self) -> QuickSettingsConfig {
        self.0.lock().unwrap().clone().unwrap_or_default()
    }
    fn save_arrangement(&self, arrangement: &QuickSettingsConfig) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(arrangement.clone());
        Ok(())
    }
    fn night_light(&self) -> bool {
        false
    }
    fn set_night_light(&self, _on: bool) -> Result<(), String> {
        Ok(())
    }
    fn rotation_lock(&self) -> bool {
        false
    }
    fn set_rotation_lock(&self, _locked: bool) -> Result<(), String> {
        Ok(())
    }
    fn rotation_available(&self) -> bool {
        false
    }
}

/// The panel mounted on `machine`'s device host, and the wakes its workers
/// send.
struct Desk {
    app: HeadlessApp<QuickSettingsApp>,
    host: DeviceHost,
    wakes: Receiver<()>,
}

impl Desk {
    fn on(machine: &Machine) -> Self {
        Self::with(machine, Kept::default())
    }

    fn with(machine: &Machine, kept: Kept) -> Self {
        let (woke, wakes) = channel();
        let host = DeviceHost::new(
            Device {
                reads: Arc::new(machine.clone()),
                dispatcher: Arc::new(machine.clone()),
                preferences: Box::new(kept),
            },
            Arc::new(move || {
                let _ = woke.send(());
            }),
        );
        let app =
            HeadlessApp::<QuickSettingsApp>::for_person_with_lent("person", Lent(host.panel()))
                .unwrap();
        let mut desk = Self { app, host, wakes };
        desk.app.harness().resize((360, 900)).unwrap();
        desk.app.settle();
        desk
    }

    /// Land what the workers deliver, as the session does on each wake,
    /// until `done` holds of the readings.
    fn until(&mut self, what: &str, done: impl Fn(&QuickSettingsState) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.host.pump();
            self.app.settle();
            if done(&self.host.state()) {
                return;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                self.wakes.recv_timeout(left).is_ok(),
                "never woken for {what}"
            );
        }
    }

    fn has(&mut self, name: &str) -> bool {
        self.app.harness().targets().contains_key(name)
    }
}

#[test]
fn the_device_is_read_off_the_ui_thread_and_lands_in_the_panel() {
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    assert!(
        !desk.has("qs-tile-vpn"),
        "nothing is read yet: no VPN, no tile"
    );
    desk.until("the first reading", |state| {
        state
            .status
            .battery
            .is_some_and(|battery| battery.percent == 64)
    });
    let state = desk.host.state();
    assert!(state.wifi && state.bluetooth && state.bluetooth_available);
    assert_eq!(state.brightness, 0.7);
    assert_eq!(state.volume_level(), 0.3);
    assert_eq!(state.power_profile, PowerProfile::Performance);
    assert_eq!(state.vpns.len(), 1);
    assert!(state.status.link.is_some(), "the top bar's link too");
    assert!(desk.has("qs-tile-vpn"), "the VPN read is a tile now");
    assert!(desk.has("qs-tile-bluetooth"));

    desk.host.set_open(true);
    desk.until("the lists opening reads", |state| {
        !state.bt_devices.is_empty() && !state.wifi_networks.is_empty()
    });
    assert_eq!(desk.host.state().firmware_updates, Some(2));

    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Bluetooth))
        .unwrap();
    desk.app.settle();
    assert!(
        desk.has("qs-accessory-00:1B:66:A1:B2:C3"),
        "the accessory read is listed"
    );
}

#[test]
fn effects_reach_the_dispatcher_and_the_device_is_read_back() {
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.bluetooth);

    desk.app
        .effect(&QsEffect::SetBluetooth { on: false })
        .unwrap();
    assert!(!desk.host.state().bluetooth, "the switch moves at once");
    desk.app
        .effect(&QsEffect::SetVolume { level: 0.5 })
        .unwrap();
    desk.app
        .effect(&QsEffect::SetPowerMode {
            mode: PowerProfile::Saver,
        })
        .unwrap();
    desk.app
        .effect(&QsEffect::SetVpn {
            profile: "Work".into(),
            on: true,
        })
        .unwrap();
    desk.until("the writes and their read-backs", |state| {
        state.power_profile == PowerProfile::Saver
            && state.volume_level() == 0.5
            && machine.calls().len() == 4
    });
    assert_eq!(
        machine.calls(),
        [
            "bluetooth:false",
            "volume:50",
            "power:Saver",
            "vpn:Work:true"
        ],
        "each change reaches the machine, in the order made"
    );
    assert!(
        !desk.host.state().bluetooth,
        "read back as the machine has it"
    );

    desk.app.effect(&QsEffect::SetWifi { on: false }).unwrap();
    desk.until("the radio change", |state| {
        !state.wifi && !state.radio_change_pending
    });
    assert_eq!(
        machine.calls().last().map(String::as_str),
        Some("wifi:false")
    );
}

#[test]
fn a_refused_change_is_told_and_the_control_goes_back() {
    let machine = Machine::laptop();
    machine.0.lock().unwrap().refuses.push("bluetooth");
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.bluetooth);
    desk.app
        .effect(&QsEffect::SetBluetooth { on: false })
        .unwrap();
    desk.until("the refusal and the read-back", |state| state.bluetooth);
    assert_eq!(
        desk.host.take_requests(),
        [SessionRequest::Notify {
            title: "Couldn’t change Bluetooth".into(),
            body: "The control was restored to the current device state.".into(),
        }]
    );
}

#[test]
fn what_only_the_desktop_can_do_comes_back_to_the_session() {
    let machine = Machine::laptop();
    let kept = Kept::default();
    let mut desk = Desk::with(&machine, kept.clone());
    for effect in [
        QsEffect::LockScreen,
        QsEffect::OpenSettings {
            link: "settings://network".into(),
        },
        QsEffect::ClearNotifications,
    ] {
        desk.app.effect(&effect).unwrap();
    }
    assert_eq!(
        desk.host.take_requests(),
        [
            SessionRequest::Effect(QsEffect::LockScreen),
            SessionRequest::Effect(QsEffect::OpenSettings {
                link: "settings://network".into(),
            }),
            SessionRequest::Effect(QsEffect::ClearNotifications),
        ]
    );
    desk.app
        .effect(&QsEffect::HideControl {
            control: "night_light".into(),
        })
        .unwrap();
    assert!(
        kept.0.lock().unwrap().as_ref().is_some_and(|arrangement| {
            !arrangement
                .controls
                .contains(&settings_store::hardware::QuickSettingsControl::NightLight)
        }),
        "the arrangement is kept"
    );
}

/// The helper's side of the line: the requests the panel sent, and a way
/// to say what plays.
struct Helper {
    say: Sender<String>,
    heard: Receiver<String>,
}

impl Helper {
    fn link() -> (PlayerLink, Self) {
        let (say, said) = channel::<String>();
        let (sent, heard) = channel::<String>();
        let link = PlayerLink::new(
            Lines(sent, Vec::new()),
            BufReader::new(Said(said, Vec::new())),
        );
        (link, Self { say, heard })
    }

    fn players(&self, players: Vec<audio::Player>) {
        self.say
            .send(audio::Event::Players { players }.line())
            .unwrap();
    }

    fn request(&self) -> audio::Request {
        let line = tilo_test_env::recv("a request to the helper", &self.heard);
        audio::Request::parse(&line).expect("a request line")
    }
}

/// Requests, a line at a time.
struct Lines(Sender<String>, Vec<u8>);

impl Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.1.extend_from_slice(bytes);
        while let Some(end) = self.1.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.1.drain(..=end).collect();
            let _ = self
                .0
                .send(String::from_utf8_lossy(&line).trim_end().to_owned());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Events, as the helper writes them.
struct Said(Receiver<String>, Vec<u8>);

impl std::io::Read for Said {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.1.is_empty() {
            match self.0.recv() {
                Ok(line) => {
                    self.1 = line.into_bytes();
                    self.1.push(b'\n');
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.1.len());
        for (slot, byte) in out.iter_mut().zip(self.1.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

fn player(bus: &str, identity: &str, status: audio::Status) -> audio::Player {
    audio::Player {
        bus_name: format!("org.mpris.MediaPlayer2.{bus}"),
        unique_name: ":1.7".into(),
        pid: None,
        package_id: None,
        desktop_entry: None,
        identity: Some(identity.into()),
        status,
        title: Some("Clair de lune".into()),
        artist: Some("Claude Debussy".into()),
        url: None,
        art_url: None,
        track_id: Some("/track/1".into()),
        length_us: Some(300_000_000),
        position_us: 72_000_000,
        rate: 1.0,
        can_play: true,
        can_pause: true,
        can_seek: true,
        can_go_next: true,
        can_go_previous: true,
    }
}

#[test]
fn what_plays_is_fed_from_the_players_and_steered_through_the_effects() {
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    let (link, helper) = Helper::link();
    desk.host.follow_players(link);
    assert!(!desk.has("qs-now-playing"), "nothing plays, no card");

    helper.players(vec![
        player("music", "Music", audio::Status::Playing),
        player("browser", "Browser", audio::Status::Paused),
    ]);
    desk.until("the players", |state| state.now_playing.is_some());
    let playing = desk.host.state().now_playing.unwrap();
    assert_eq!(playing.title, "Clair de lune");
    assert_eq!(playing.player_name, "Music");
    assert!(playing.playing);
    assert!(desk.has("qs-now-playing"));
    assert!(
        desk.has("qs-now-playing-player"),
        "two players to choose from"
    );

    desk.app.harness().click_named("transport.play").unwrap();
    desk.app.settle();
    assert_eq!(
        helper.request(),
        audio::Request::Control {
            bus_name: "org.mpris.MediaPlayer2.music".into(),
            command: audio::PlayerCommand::PlayPause,
        },
        "the press goes to the player in front"
    );
    assert!(
        !desk.host.state().now_playing.unwrap().playing,
        "shown paused at once"
    );

    desk.app.effect(&QsEffect::Seek { fraction: 0.5 }).unwrap();
    assert_eq!(
        helper.request(),
        audio::Request::Control {
            bus_name: "org.mpris.MediaPlayer2.music".into(),
            command: audio::PlayerCommand::SetPosition {
                track_id: "/track/1".into(),
                position_us: 150_000_000,
            },
        }
    );

    desk.app
        .effect(&QsEffect::ChoosePlayer {
            player: "org.mpris.MediaPlayer2.browser".into(),
        })
        .unwrap();
    assert_eq!(
        desk.host.state().now_playing.unwrap().player_name,
        "Browser"
    );
    desk.app.effect(&QsEffect::NextTrack).unwrap();
    assert_eq!(
        helper.request(),
        audio::Request::Control {
            bus_name: "org.mpris.MediaPlayer2.browser".into(),
            command: audio::PlayerCommand::Next,
        },
        "the chosen player is the one steered"
    );

    helper
        .say
        .send(audio::Event::Device { connected: true }.line())
        .unwrap();
    drop(helper);
    desk.until("the helper going", |state| state.now_playing.is_none());
    assert!(
        desk.host
            .take_requests()
            .contains(&SessionRequest::AudioDevice { connected: true }),
        "an audio device arriving is the session's to sound"
    );
}
