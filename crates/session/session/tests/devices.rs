//! The person's other devices in quick settings: the fabric's devices become
//! the Devices section's rows and the places an accessory can move to, and a
//! press on one sends it what plays here, or opens the device's settings.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{BufReader, Read};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::DoorError;
use tilo_actions::remote::{RemoteActionTarget, application_action_target};
use tilo_compositor::OutputConfig;
use tilo_desktop::desktop_actions::CHROME_RESOURCE;
use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError, SharedLink};
use tilo_device_fabric::speaker_group::SpeakerFabricState;
use tilo_device_fabric::unlock::UnlockFabricState;
use tilo_device_fabric::{
    DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
    FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, MediaSourceLocator, OwnerSession,
    PersonalFabricAction,
};
use tilo_quick_settings::device::Device;
use tilo_quick_settings::device::PlayerLink;
use tilo_quick_settings::device::testing::{Kept, Machine};
use tilo_quick_settings::hosted::HostedActions;
use tilo_quick_settings::{QsEffect, RowPart};
use tilo_session::{AppActions, AppLists, DeviceHost, Lists, Options, QuickSettings, Session};

type Arguments = serde_json::Map<String, serde_json::Value>;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// This device's fabric id.
fn this_device() -> DeviceId {
    DeviceId::new("this-pc").expect("a device id")
}

/// The other of the person's devices, online with this one.
fn laptop() -> DeviceId {
    DeviceId::new("laptop").expect("a device id")
}

/// The fabric as a stand-in transport keeps it, and what the session sent
/// through it.
#[derive(Default)]
struct Fabric {
    converged: DeviceFabric,
    sent: Vec<(DeviceId, PersonalFabricAction)>,
}

#[derive(Clone, Default)]
struct FakeLink {
    fabric: Arc<Mutex<Fabric>>,
    identity: Arc<Mutex<Option<DeviceId>>>,
    /// Whether the person lets media move between their devices.
    media: Arc<Mutex<bool>>,
}

impl FakeLink {
    /// A link on a fabric this device has joined, with `laptop` online too
    /// and showing (a monitor, when `display`).
    fn joined(display: bool) -> Self {
        let link = Self::default();
        *link.identity.lock().expect("the identity") = Some(this_device());
        *link.media.lock().expect("the media switch") = true;
        link.joined_device(this_device(), "This computer", true, true);
        link.joined_device(laptop(), "Laptop", true, display);
        link
    }

    /// A device of the person's, as it said of itself.
    fn joined_device(&self, device: DeviceId, label: &str, online: bool, display: bool) {
        let stamp = FabricStamp {
            counter: 1,
            device: device.clone(),
        };
        let snapshot = DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: device.clone(),
            presence: DevicePresence {
                device,
                label: label.to_owned(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities {
                    display,
                    audio_output: true,
                    ..FabricCapabilities::default()
                },
                online,
                last_seen_unix_ms: 0,
                wake_routes: Vec::new(),
                session: OwnerSession::default(),
                stamp,
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
            speakers: SpeakerFabricState::default(),
            unlock: UnlockFabricState::default(),
        };
        self.fabric
            .lock()
            .expect("the fabric")
            .converged
            .upsert(snapshot)
            .expect("the snapshot is valid");
    }

    /// What the session sent through the fabric.
    fn sent(&self) -> Vec<(DeviceId, PersonalFabricAction)> {
        self.fabric.lock().expect("the fabric").sent.clone()
    }

    fn set_media(&self, on: bool) {
        *self.media.lock().expect("the media switch") = on;
    }
}

impl FabricLink for FakeLink {
    fn device_fabric(&self) -> DeviceFabric {
        self.fabric.lock().expect("the fabric").converged.clone()
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        BTreeSet::new()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        self.identity
            .lock()
            .expect("the identity")
            .clone()
            .map(|device| (device, "this computer".to_owned()))
    }

    fn send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], String> {
        self.try_send_personal_fabric_action(target, action)
            .map_err(|error| error.to_string())
    }

    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        self.fabric
            .lock()
            .expect("the fabric")
            .sent
            .push((target.clone(), action));
        Ok([0; 16])
    }

    fn require_media(&self) -> Result<(), String> {
        (*self.media.lock().expect("the media switch"))
            .then_some(())
            .ok_or_else(|| "media continuity is off".to_owned())
    }
}

/// An action host that keeps what it was asked to run.
#[derive(Default)]
struct Host {
    invoked: RefCell<Vec<(RemoteActionTarget, Arguments)>>,
}

impl AppActions for Host {
    fn published(&self) -> Vec<PublishedActions> {
        Vec::new()
    }

    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    fn invoke(&self, target: RemoteActionTarget, arguments: Arguments) -> Result<(), DoorError> {
        self.invoked.borrow_mut().push((target, arguments));
        Ok(())
    }

    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn open_uri(&self, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn start_host_app(&self, _: &str) -> Result<(), DoorError> {
        Ok(())
    }
}

/// The session and the device host behind its panel, which the test drives
/// the players through.
struct Harness {
    session: Session,
    device: Rc<RefCell<Option<DeviceHost>>>,
    /// The lines the audio helper writes: what plays here.
    plays: Sender<String>,
}

fn session(link: &FakeLink, host: Rc<Host>) -> Harness {
    let reads = Machine::laptop();
    let fabric: SharedLink = Arc::new(link.clone());
    let held: Rc<RefCell<Option<DeviceHost>>> = Rc::default();
    let given = Rc::clone(&held);
    let (lines, heard) = channel();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            quick_settings: Some(QuickSettings::Device(Box::new(move |wake| {
                let device = DeviceHost::new(
                    Device {
                        reads: std::sync::Arc::new(reads.clone()),
                        dispatcher: std::sync::Arc::new(reads),
                        preferences: Box::new(Kept::default()),
                    },
                    wake,
                );
                device.follow_players(PlayerLink::new(
                    std::io::sink(),
                    BufReader::new(Said(heard, Vec::new())),
                ));
                *given.borrow_mut() = Some(device.clone());
                device
            }))),
            search: tilo_session::SearchOptions {
                lists: Lists::fixed(AppLists {
                    applications: vec![AppEntry {
                        id: "so.tilo.settings".into(),
                        title: "Settings".into(),
                        category: None,
                        description: None,
                    }],
                    ..AppLists::default()
                }),
                ..tilo_session::SearchOptions::default()
            },
            apps: Some(host),
            fabric: Some(fabric),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    let device = held.borrow().clone().expect("the panel's device host");
    Harness {
        session,
        device: Rc::new(RefCell::new(Some(device))),
        plays: lines,
    }
}

impl Harness {
    fn device(&self) -> DeviceHost {
        self.device
            .borrow()
            .clone()
            .expect("the panel's device host")
    }

    /// What plays here: a browser stream, playing at 1:12.
    fn playing(&self) {
        self.plays.send(player().line()).expect("the helper reads");
    }

    fn step(&mut self) {
        self.session.step(Duration::from_millis(1)).expect("step");
    }

    fn press(&mut self, row: &str, part: RowPart) {
        self.session
            .invoke(
                tilo_session::QUICK_SETTINGS,
                QsEffect::PRESS_DEVICE.id,
                &serde_json::from_value(serde_json::json!({ "row": row, "part": part }))
                    .expect("arguments"),
            )
            .expect("press the device's row");
        for _ in 0..3 {
            self.step();
        }
    }
}

#[test]
fn the_person_s_devices_become_rows_and_where_an_accessory_can_move() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(true);
    let mut harness = session(&link, Rc::new(Host::default()));
    harness
        .session
        .invoke(CHROME_RESOURCE, "toggle-quick-settings", &Arguments::new())
        .expect("open quick settings");
    harness.playing();
    for _ in 0..40 {
        harness.step();
        if !harness.device().state().device_rows.is_empty() {
            break;
        }
    }
    let state = harness.device().state();
    assert_eq!(
        state
            .device_rows
            .iter()
            .map(|row| (row.key.as_str(), row.title.as_str(), row.detail.as_str()))
            .collect::<Vec<_>>(),
        [("laptop", "Laptop", "Online")],
        "one row per device other than this one"
    );
    assert_eq!(
        state.device_rows[0].actions,
        HostedActions::Select("Play Clair de lune here".into()),
        "the row says what pressing it does"
    );
    assert_eq!(state.personal_devices, Some(2), "this one counts too");
    assert_eq!(
        state
            .handoff_targets
            .as_deref()
            .expect("the fabric's devices")
            .iter()
            .map(|target| (target.id.as_str(), target.online, target.local))
            .collect::<Vec<_>>(),
        [("this-pc", true, true), ("laptop", true, false)],
        "this device is listed too, and says it is this one"
    );
}

#[test]
fn pressing_another_device_sends_it_what_plays_here() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(true);
    let mut harness = session(&link, Rc::new(Host::default()));
    harness.playing();
    for _ in 0..4 {
        harness.step();
    }
    harness.press("laptop", RowPart::Action);

    let sent = link.sent();
    assert_eq!(sent.len(), 1, "one action, not two: {sent:?}");
    let (target, action) = sent.first().expect("the hand-over");
    assert_eq!(target, &laptop(), "the device pressed");
    let PersonalFabricAction::ResumeMedia {
        session_id,
        app_id,
        title,
        locator,
        position_ms,
        resume_playing,
        has_video,
        ..
    } = action
    else {
        panic!("not a hand-over: {action:?}");
    };
    assert_eq!(app_id, "so.tilo.music");
    assert_eq!(title, "Clair de lune");
    assert_eq!(
        locator,
        &MediaSourceLocator::RemoteStream {
            url: "https://media.example/clair.flac".into()
        }
    );
    assert!(
        session_id.starts_with("so.tilo.music:mpris:"),
        "{session_id}"
    );
    assert!(
        (72_000..=75_000).contains(position_ms),
        "where it stood: {position_ms}"
    );
    assert!(*resume_playing);
    assert!(!has_video);
}

#[test]
fn pressing_a_device_with_nothing_playing_opens_its_settings() {
    let _settled = ui::motion::testing::settled();
    let host = Rc::new(Host::default());
    let link = FakeLink::joined(false);
    let mut harness = session(&link, Rc::clone(&host));
    harness
        .session
        .invoke(CHROME_RESOURCE, "toggle-quick-settings", &Arguments::new())
        .expect("open quick settings");
    for _ in 0..4 {
        harness.step();
    }
    harness.press("laptop", RowPart::Row);

    assert!(link.sent().is_empty(), "nothing plays: nothing is sent");
    assert!(
        !harness.session.quick_settings_open(),
        "the panel closes on the way to Settings"
    );
    let room = harness.session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.room().to_string())
            .unwrap_or_default()
    });
    assert_eq!(
        host.invoked
            .borrow()
            .first()
            .map(|(target, _)| target.clone()),
        Some(application_action_target(
            &room,
            "so.tilo.settings",
            "settings.section:devices_sync",
            "open"
        )),
        "a device without a screen is managed on the devices page"
    );

    // With media continuity turned off, what plays here stays here: the press
    // opens the device's settings instead.
    let host = Rc::new(Host::default());
    let link = FakeLink::joined(false);
    link.set_media(false);
    let mut harness = session(&link, Rc::clone(&host));
    harness.playing();
    for _ in 0..4 {
        harness.step();
    }
    harness.press("laptop", RowPart::Action);
    assert!(link.sent().is_empty(), "media continuity is off");
    assert!(
        !host.invoked.borrow().is_empty(),
        "its settings page opens instead"
    );

    // A device that shows has its display placed beside this one's, so its
    // press opens that page.
    let link = FakeLink::joined(true);
    let host = Rc::new(Host::default());
    let mut harness = session(&link, Rc::clone(&host));
    for _ in 0..2 {
        harness.step();
    }
    harness.press("laptop", RowPart::Row);
    assert!(link.sent().is_empty(), "nothing plays: nothing is sent");
    assert!(
        host.invoked
            .borrow()
            .iter()
            .any(|(target, _)| format!("{target:?}").contains("settings.section:displays")),
        "a device that shows opens the displays page"
    );
}

#[test]
fn this_device_s_row_does_nothing() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(true);
    let host = Rc::new(Host::default());
    let mut harness = session(&link, Rc::clone(&host));
    harness.playing();
    for _ in 0..4 {
        harness.step();
    }
    harness.press("this-pc", RowPart::Row);
    assert!(link.sent().is_empty());
    assert!(host.invoked.borrow().is_empty());
}

#[test]
fn a_row_a_device_left_behind_is_a_press_on_nothing() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(true);
    let host = Rc::new(Host::default());
    let mut harness = session(&link, Rc::clone(&host));
    harness.playing();
    for _ in 0..4 {
        harness.step();
    }
    harness.press("phone", RowPart::Row);
    assert!(link.sent().is_empty());
    assert!(host.invoked.borrow().is_empty());
}

/// What the helper says plays here: a browser stream.
fn player() -> tilo_audio_service::Event {
    tilo_audio_service::Event::Players {
        players: vec![tilo_audio_service::Player {
            bus_name: "org.mpris.MediaPlayer2.browser".into(),
            unique_name: ":1.7".into(),
            pid: None,
            package_id: Some("so.tilo.music".into()),
            desktop_entry: None,
            identity: Some("Music".into()),
            status: tilo_audio_service::Status::Playing,
            title: Some("Clair de lune".into()),
            artist: Some("Claude Debussy".into()),
            url: Some("https://media.example/clair.flac".into()),
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
        }],
    }
}

/// Events, as the helper writes them.
struct Said(Receiver<String>, Vec<u8>);

impl Said {
    fn fill(&mut self) -> bool {
        match self.0.recv() {
            Ok(line) => {
                self.1.extend_from_slice(line.as_bytes());
                self.1.push(b'\n');
                true
            }
            Err(_) => false,
        }
    }
}

impl Read for Said {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.1.is_empty() {
            if !self.fill() {
                return Ok(0);
            }
        }
        let n = self.1.len().min(buf.len());
        buf[..n].copy_from_slice(&self.1[..n]);
        self.1.drain(..n);
        Ok(n)
    }
}
