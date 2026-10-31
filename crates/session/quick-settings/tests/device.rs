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
    MediaOffer, PowerProfile, QsEffect, QsSection, QuickSettings, QuickSettingsApp,
    QuickSettingsState,
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
    assert_eq!(state.power_profile, PowerProfile::Performance);
    assert_eq!(state.vpns.len(), 1);
    assert!(state.status.link.is_some(), "the top bar's link too");
    assert!(
        !state.volume_available,
        "the sound is the audio service's to read, and none published"
    );
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
        state.power_profile == PowerProfile::Saver && machine.calls().len() == 3
    });
    assert_eq!(
        machine.calls(),
        ["bluetooth:false", "power:Saver", "vpn:Work:true"],
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
fn the_sound_is_what_the_audio_service_reads_and_it_leaves_the_panel() {
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.bluetooth);
    desk.host.sound(&sound());

    let state = desk.host.state();
    assert!(state.volume_available);
    assert_eq!(state.volume_level(), 0.25);
    assert!(state.volume_muted());
    assert_eq!(
        state
            .sound_outputs
            .iter()
            .map(|output| (output.node, output.current))
            .collect::<Vec<_>>(),
        [(40, true), (41, false)],
        "the outputs and the one in use"
    );

    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Sound))
        .unwrap();
    desk.app.settle();
    assert!(desk.has("qs-output-40"), "the output in use is listed");
    assert!(desk.has("qs-output-41"), "and the one to switch to");

    // Every sound change is the audio service's to make, so the panel asks
    // the session for it and shows where the person put it meanwhile.
    for effect in [
        QsEffect::SetVolume { level: 0.5 },
        QsEffect::ToggleMuted,
        QsEffect::RouteSound { node: 41 },
    ] {
        desk.app.effect(&effect).unwrap();
    }
    assert_eq!(desk.host.state().volume_level(), 0.5, "the slider moved");
    assert_eq!(
        desk.host.take_requests(),
        [
            SessionRequest::Effect(QsEffect::SetVolume { level: 0.5 }),
            SessionRequest::Effect(QsEffect::ToggleMuted),
            SessionRequest::Effect(QsEffect::RouteSound { node: 41 }),
        ]
    );
    assert!(
        machine.calls().is_empty(),
        "the panel no longer writes the sound itself: {:?}",
        machine.calls()
    );

    // The service's reading is the truth, even against the guess the panel
    // showed while the change was under way.
    let mut after = sound();
    after.volume = Some(0.45);
    after.muted = false;
    desk.host.sound(&after);
    assert_eq!(desk.host.state().volume_level(), 0.45);
    assert!(!desk.host.state().volume_muted());
}

#[test]
fn the_microphone_row_mutes_and_sets_its_level() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.bluetooth);
    desk.host.sound(&sound());
    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Sound))
        .unwrap();
    desk.app.settle();
    assert!(desk.has("qs-input-50"), "the microphone that records");
    assert!(desk.has("qs-input-mute"), "the button that mutes it");
    assert!(desk.has("qs-input-level"), "and its level");

    desk.app
        .effect(&QsEffect::SetInputVolume { level: 0.6 })
        .unwrap();
    desk.app.harness().click_named("qs-input-mute").unwrap();
    desk.app.settle();
    let microphone = desk.host.state().microphone.expect("still recording");
    assert_eq!(microphone.level, 0.6, "the slider moved");
    assert!(!microphone.muted, "a press unmuted it");

    // Both are the audio service's to carry out: the panel asks the
    // session and never writes the graph itself.
    assert_eq!(
        desk.host.take_requests(),
        [
            SessionRequest::Effect(QsEffect::SetInputVolume { level: 0.6 }),
            SessionRequest::Effect(QsEffect::ToggleInputMuted),
        ]
    );
    assert!(
        machine.calls().is_empty(),
        "the panel never writes the microphone itself: {:?}",
        machine.calls()
    );
}

#[test]
fn an_apps_stream_is_moved_and_its_gain_set() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.bluetooth);
    desk.host.sound(&playing());
    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Sound))
        .unwrap();
    desk.app.settle();
    assert!(desk.has("qs-stream-60"), "the application's stream");
    assert!(desk.has("qs-stream-60-mute"), "the button that mutes it");
    assert!(desk.has("qs-stream-60-gain"), "its gain");
    assert!(desk.has("qs-stream-60-output-40"), "where it plays now");
    assert!(desk.has("qs-stream-60-output-41"), "and where it can go");
    assert!(
        desk.app.harness().a11y_nodes().iter().any(|(_, node)| node
            .value()
            .is_some_and(|value| value.contains("Playing on Speakers"))),
        "the row says where the stream goes"
    );

    desk.app
        .harness()
        .click_named("qs-stream-60-output-41")
        .unwrap();
    desk.app
        .effect(&QsEffect::SetStreamVolume {
            stream: 60,
            level: 0.4,
        })
        .unwrap();
    desk.app.settle();
    assert_eq!(
        desk.host.state().sound_streams[0].level,
        0.4,
        "the gain moved"
    );

    // Moving it and setting its gain are the audio service's: the panel
    // asks the session for both and leaves the graph alone.
    assert_eq!(
        desk.host.take_requests(),
        [
            SessionRequest::Effect(QsEffect::RouteStream {
                stream: 60,
                device: Some(41),
            }),
            SessionRequest::Effect(QsEffect::SetStreamVolume {
                stream: 60,
                level: 0.4,
            }),
        ]
    );
    assert!(
        machine.calls().is_empty(),
        "the panel never writes the stream itself: {:?}",
        machine.calls()
    );
}

/// A reading like the audio service publishes: speakers playing quietly
/// and muted, headphones to switch to, a muted microphone.
fn sound() -> audio::sound::Sound {
    use audio::sound::Device;
    audio::sound::Sound {
        output: Some(40),
        volume: Some(0.25),
        muted: true,
        input: Some(50),
        input_muted: true,
        outputs: vec![
            Device {
                node: 40,
                name: "node-40".into(),
                label: "Speakers".into(),
                chosen: true,
                volume: Some(0.25),
                muted: true,
                channels: 2,
                codec: None,
            },
            Device {
                node: 41,
                name: "node-41".into(),
                label: "Headphones".into(),
                chosen: false,
                volume: None,
                muted: false,
                channels: 2,
                codec: None,
            },
        ],
        inputs: vec![Device {
            node: 50,
            name: "node-50".into(),
            label: "Microphone".into(),
            chosen: true,
            volume: None,
            muted: true,
            channels: 1,
            codec: None,
        }],
        ..audio::sound::Sound::default()
    }
}

/// The same reading with an application playing: its stream on the
/// speakers, with a gain of its own.
fn playing() -> audio::sound::Sound {
    audio::sound::Sound {
        streams: vec![audio::sound::Stream {
            node: 60,
            app: "Music".into(),
            media: Some("Clair de lune".into()),
            playback: true,
            target: Some(40),
            volume: Some(0.3),
            muted: false,
        }],
        ..sound()
    }
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
fn saved_networks_are_kept_from_joining_by_themselves_and_forgotten() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.wifi);
    desk.host.set_open(true);
    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Wifi))
        .unwrap();
    desk.app.settle();
    desk.until("the section's reads", |state| {
        !state.wifi_networks.is_empty() && !state.saved_networks.is_empty()
    });
    assert!(
        desk.has("qs-saved-network-grandma-profile"),
        "a saved network out of reach is still listed"
    );
    assert!(
        desk.has("qs-forget-network-home-profile"),
        "the network in use can be forgotten from its own row"
    );

    desk.app
        .harness()
        .click_named("qs-autoconnect-grandma-profile")
        .unwrap();
    desk.until("the autoconnect change and its read-back", |state| {
        state
            .saved_networks
            .iter()
            .any(|saved| saved.profile == "grandma-profile" && !saved.autoconnect)
    });

    desk.app
        .harness()
        .click_named("qs-forget-network-grandma-profile")
        .unwrap();
    desk.app.settle();
    assert!(
        desk.app.confirm_asked().is_some(),
        "the person agrees to forget it"
    );
    desk.app.settle();
    desk.until("the forgetting and its read-back", |state| {
        !state
            .saved_networks
            .iter()
            .any(|saved| saved.profile == "grandma-profile")
    });
    assert!(
        !desk.has("qs-saved-network-grandma-profile"),
        "forgotten, it is gone from the section"
    );
    assert_eq!(
        machine.calls(),
        [
            "autoconnect:grandma-profile:false",
            "forget-network:grandma-profile"
        ]
    );
}

#[test]
fn a_hidden_network_is_joined_by_its_name_and_password() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    desk.until("the first reading", |state| state.wifi);
    desk.host.set_open(true);
    desk.app
        .invoke(QuickSettings::OPEN_SECTION.with(QsSection::Wifi))
        .unwrap();
    desk.app.settle();
    desk.until("the section's reads", |state| {
        !state.wifi_networks.is_empty()
    });
    assert!(desk.has("qs-wifi-hidden"), "a way into a hidden network");

    desk.app.effect(&QsEffect::AskHiddenNetwork).unwrap();
    desk.app.settle();
    assert!(
        desk.has("qs-wifi-hidden-form"),
        "the form asks for its name and password"
    );

    desk.app
        .effect(&QsEffect::ConnectHiddenWifi {
            network: "Basement".into(),
            password: "hunter22".into(),
        })
        .unwrap();
    assert!(
        desk.host
            .state()
            .wifi_hidden
            .is_some_and(|prompt| prompt.connecting),
        "the form says it is joining"
    );
    desk.until("the join and its read-back", |state| {
        state.wifi_joining.is_none()
    });
    assert!(
        desk.host.state().wifi_hidden.is_none(),
        "joined, the form is gone"
    );
    assert_eq!(machine.calls(), ["join-hidden:Basement:hunter22"]);
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

#[test]
fn the_cards_destinations_are_the_machines_outputs_and_choosing_one_is_the_sessions() {
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    let (link, helper) = Helper::link();
    desk.host.follow_players(link);
    helper.players(vec![player("music", "Music", audio::Status::Playing)]);
    desk.until("the players", |state| state.now_playing.is_some());
    assert!(
        !desk.has("qs-now-playing-output"),
        "nothing has read where the sound can go"
    );

    // The audio service's reading is where the card's destinations come
    // from, the output that plays marked.
    desk.host.sound(&sound());
    desk.app.settle();
    let playing = desk.host.state().now_playing.expect("still playing");
    assert_eq!(
        playing
            .outputs
            .iter()
            .map(|output| (output.label.as_str(), output.current))
            .collect::<Vec<_>>(),
        [("Speakers", true), ("Headphones", false)]
    );
    assert!(
        desk.has("qs-now-playing-output"),
        "the outputs to choose from"
    );

    // Choosing one is the session's to carry out through the audio
    // service: the card names the output and never writes the graph.
    let choose = QsEffect::ChooseOutput {
        output: audio::sound::output_id(41),
    };
    desk.app.effect(&choose).unwrap();
    assert_eq!(
        desk.host.take_requests(),
        [SessionRequest::Effect(choose)],
        "the card asks the session"
    );
    assert!(
        helper.heard.try_recv().is_err(),
        "and never the players' helper"
    );
}

#[test]
fn resuming_the_other_device_s_playhead_is_asked_from_the_card() {
    let machine = Machine::laptop();
    let mut desk = Desk::on(&machine);
    let (link, helper) = Helper::link();
    desk.host.follow_players(link);

    // Another of the person's devices hands this one what it played, while
    // nothing plays here: the card is that offer, saying what it is.
    desk.host.handoff(Some(MediaOffer {
        request_id: "00112233445566778899aabbccddeeff".into(),
        package_id: "so.tilo.music".into(),
        title: "Clair de lune".into(),
        source_label: "Studio".into(),
        source: media::handoff::HandoffSource::RemoteStream {
            url: "https://media.example/clair.flac".into(),
        },
        position_ms: 83_000.0,
        playing: true,
    }));
    desk.app.settle();
    assert!(
        desk.has("qs-now-playing-resume"),
        "the card offers to continue it here"
    );
    let said = "From Studio: Clair de lune, resumes at 1:23";
    assert!(
        desk.app.harness().a11y_node_named(said).is_some(),
        "the row is read as the session another device hands over: {said:?}"
    );

    // Pressing it is the helper's to carry out, with the session's exact
    // request, app, source and playhead.
    desk.app
        .harness()
        .click_named("qs-now-playing-resume")
        .unwrap();
    desk.app.settle();
    assert_eq!(
        helper.request(),
        audio::Request::Resume {
            request: audio::ResumeRequest {
                request_id: [
                    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
                    0xdd, 0xee, 0xff,
                ],
                package_id: "so.tilo.music".into(),
                source: audio::ResumeSource::Uri("https://media.example/clair.flac".into()),
                position_ms: 83_000,
                playing: true,
            }
        }
    );
    assert!(helper.heard.try_recv().is_err(), "a resume is one request");
    assert!(
        !desk.has("qs-now-playing-resume"),
        "continued, nothing is left to offer"
    );
}
