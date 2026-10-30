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
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use tilo_quick_settings::device::testing::{Kept, Machine};
use tilo_quick_settings::device::{Device, DeviceHost, PlayerLink, SessionRequest};
use tilo_quick_settings::host::Lent;
use tilo_quick_settings::{
    PowerProfile, QsEffect, QsSection, QuickSettings, QuickSettingsApp, QuickSettingsState,
};
use ui::framework::testing::HeadlessApp;

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
    machine.refuse("bluetooth");
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
        kept.saved().as_ref().is_some_and(|arrangement| {
            !arrangement
                .controls
                .contains(&settings_store::hardware::QuickSettingsControl::NightLight)
        }),
        "the arrangement is kept"
    );
}

#[test]
fn the_wired_link_is_switched_from_the_network_section() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let machine = Machine::desk();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| {
        state.wired.as_ref().is_some_and(|wired| wired.connected)
    });
    desk.host.set_open(true);
    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Wifi))
        .unwrap();
    desk.app.settle();
    desk.until("the open section", |state| state.wired.is_some());
    assert!(desk.has("qs-wired"), "the cable's row");
    assert!(
        desk.app.harness().targets().contains_key("qs-wired-switch"),
        "a switch for the cable"
    );

    desk.app.harness().click_named("qs-wired-switch").unwrap();
    assert!(
        !desk.host.state().wired.unwrap_or_default().connected,
        "it goes at once"
    );
    desk.until("the wired change and its read-back", |state| {
        state.wired.as_ref().is_some_and(|wired| !wired.connected)
    });
    assert_eq!(machine.calls(), ["wired:false"]);
}

#[test]
fn bluetooth_devices_are_looked_for_paired_and_forgotten_in_the_section() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.bluetooth);
    desk.host.set_open(true);
    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Bluetooth))
        .unwrap();
    desk.app.settle();
    desk.until("the section's reads", |state| !state.bt_nearby.is_empty());
    assert!(desk.has("qs-bluetooth-look"), "a row to look for devices");
    assert!(
        desk.has("qs-nearby-AA:CC:DD:EE:FF:00"),
        "the speaker in reach is listed"
    );

    desk.app
        .effect(&QsEffect::ScanBluetooth { on: true })
        .unwrap();
    assert!(desk.host.state().bt_scanning, "looking at once");
    desk.until("the radio looking", |state| state.bt_scanning);

    desk.app
        .effect(&QsEffect::PairBluetooth {
            address: "AA:CC:DD:EE:FF:00".into(),
        })
        .unwrap();
    desk.until("the pairing and its read-back", |state| {
        state.bt_nearby.is_empty()
            && state
                .bt_devices
                .iter()
                .any(|device| device.address == "AA:CC:DD:EE:FF:00")
    });
    assert!(
        desk.has("qs-accessory-AA:CC:DD:EE:FF:00"),
        "paired, it is listed as an accessory"
    );

    desk.app
        .harness()
        .click_named("qs-bluetooth-forget-AA:CC:DD:EE:FF:00")
        .unwrap();
    desk.app.settle();
    assert!(
        desk.app.confirm_asked().is_some(),
        "the person agrees to forget it"
    );
    desk.app.settle();
    desk.until("the forgetting and its read-back", |state| {
        !state
            .bt_devices
            .iter()
            .any(|device| device.address == "AA:CC:DD:EE:FF:00")
    });
    assert!(!desk.has("qs-accessory-AA:CC:DD:EE:FF:00"), "it is gone");
    assert_eq!(
        machine.calls(),
        [
            "scan:true",
            "pair:AA:CC:DD:EE:FF:00",
            "forget:AA:CC:DD:EE:FF:00"
        ]
    );
}

#[test]
fn leaving_the_bluetooth_section_stops_looking_for_devices() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.bluetooth);
    desk.host.set_open(true);
    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Bluetooth))
        .unwrap();
    desk.app
        .effect(&QsEffect::ScanBluetooth { on: true })
        .unwrap();
    desk.until("the radio looking", |state| state.bt_scanning);

    desk.app
        .invoke(QuickSettings::TOGGLE_SECTION.with(QsSection::Bluetooth))
        .unwrap();
    desk.app.settle();
    desk.until("the radio stopping", |state| !state.bt_scanning);
    assert_eq!(
        machine.calls(),
        ["scan:true", "scan:false"],
        "nobody looks for devices no longer listed"
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
