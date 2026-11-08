//! The person's other devices in quick settings: the fabric's devices become the Devices section's
//! rows and the places an accessory can move to, and a press on one sends it what plays here, or
//! opens the device's settings.

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

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_actions::DoorError;
use tilo_actions::remote::{RemoteActionTarget, application_action_target};
use tilo_compositor::OutputConfig;
use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError, SharedLink};
use tilo_device_fabric::speaker_group::SpeakerFabricState;
use tilo_device_fabric::unlock::UnlockFabricState;
use tilo_device_fabric::wake::{self, MagicPackets};
use tilo_device_fabric::{
    DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
    FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, Ipv4Subnet, MediaSourceLocator,
    OwnerSession, PersonalFabricAction, PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS,
    REMOTE_AUDIO_FRAME_SAMPLES, REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_RATE_HZ,
    WakeInterfaceKind, WakeRoute,
};
use tilo_quick_settings::RowPart;
use tilo_quick_settings::hosted::{ASK_KEY, DevicesReading, HostedActions, ROUTE_KEY, SOUND_KEY};
use tilo_session::surfaces::Surface;
use tilo_session::{AppActions, AppLists, Lists, Options, Session, WakeInterfaces, WakeSeams};

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

/// A device of the person's asleep off the network.
fn desk() -> DeviceId {
    DeviceId::new("desk").expect("a device id")
}

/// An awake device of the person's on the sleeping one's network.
fn server() -> DeviceId {
    DeviceId::new("server").expect("a device id")
}

/// The network the sleeping device is on.
const LAN: &str = "192.168.1.0/24";
/// The network this machine is on, away from home.
const AWAY: &str = "10.0.0.0/24";

fn subnet(text: &str) -> Ipv4Subnet {
    let (address, prefix) = text.split_once('/').expect("a subnet");
    Ipv4Subnet::containing(
        address.parse().expect("an address"),
        prefix.parse().expect("a prefix"),
    )
    .expect("a subnet")
}

fn wake_route(mac: u8, on: &str) -> WakeRoute {
    WakeRoute {
        mac: [0x52, 0x54, 0, 0, 0, mac],
        kind: WakeInterfaceKind::Ethernet,
        magic_packet: true,
        subnet: Some(subnet(on)),
    }
}

/// The packets that left, as they went out.
#[derive(Default)]
struct Packets(Mutex<Vec<([u8; 102], Ipv4Subnet)>>);

impl Packets {
    fn taken(&self) -> Vec<([u8; 102], Ipv4Subnet)> {
        self.0.lock().expect("the packets").clone()
    }
}

impl MagicPackets for Packets {
    fn send(&self, mac: [u8; 6], subnet: Ipv4Subnet) -> std::io::Result<()> {
        self.0
            .lock()
            .expect("the packets")
            .push((wake::magic_packet(mac), subnet));
        Ok(())
    }
}

/// This machine's interfaces, fixed so the test does not depend on the machine running it.
struct FixedInterfaces(Vec<WakeRoute>);

impl WakeInterfaces for FixedInterfaces {
    fn routes(&self) -> Vec<WakeRoute> {
        self.0.clone()
    }
}

/// The fabric as a stand-in transport keeps it, and what the session sent through it.
#[derive(Default)]
struct Fabric {
    converged: DeviceFabric,
    sent: Vec<(DeviceId, PersonalFabricAction)>,
    /// What this device's session said of itself, as its record carries it.
    published: Option<OwnerSession>,
    /// The revision the next record a device says of itself is said at.
    counter: u64,
}

#[derive(Clone, Default)]
struct FakeLink {
    fabric: Arc<Mutex<Fabric>>,
    identity: Arc<Mutex<Option<DeviceId>>>,
    /// Whether the person lets media move between their devices.
    media: Arc<Mutex<bool>>,
    /// The person's devices this session has a live session with.
    online: Arc<Mutex<BTreeSet<DeviceId>>>,
    /// The wake routes this machine offered its devices.
    routes: Arc<Mutex<Vec<WakeRoute>>>,
}

impl FakeLink {
    /// A link on a fabric this device has joined, with `laptop` online too and showing (a monitor,
    /// when `display` ).
    fn joined(display: bool) -> Self {
        let link = Self::default();
        *link.identity.lock().expect("the identity") = Some(this_device());
        *link.media.lock().expect("the media switch") = true;
        link.joined_device(this_device(), "This computer", true, true);
        link.joined_device(laptop(), "Laptop", true, display);
        link
    }

    /// A device of the person's, awake now, reporting that `mac` on `on` wakes it.
    fn joined_awake_device(&self, device: DeviceId, label: &str, mac: u8, on: &str) {
        self.put_device(
            device,
            label,
            true,
            false,
            vec![wake_route(mac, on)],
            OwnerSession::default(),
        );
    }

    /// A device of the person's, asleep off the network, reporting that `mac` on `on` wakes it.
    fn joined_sleeping_device(&self, device: DeviceId, label: &str, mac: u8, on: &str) {
        self.put_device(
            device,
            label,
            false,
            false,
            vec![wake_route(mac, on)],
            OwnerSession::default(),
        );
    }

    /// The wake routes this machine offered its devices.
    fn shared_routes(&self) -> Vec<WakeRoute> {
        self.routes.lock().expect("the routes").clone()
    }

    /// A device of the person's, as it said of itself.
    fn joined_device(&self, device: DeviceId, label: &str, online: bool, display: bool) {
        self.put_device(
            device,
            label,
            online,
            display,
            Vec::new(),
            OwnerSession::default(),
        );
    }

    /// Both of the person's devices on their home network, which is what lets one follow a lock on
    /// the other.
    fn joined_at_home(display: bool) -> Self {
        let link = Self::default();
        *link.identity.lock().expect("the identity") = Some(this_device());
        *link.media.lock().expect("the media switch") = true;
        link.put_device(
            this_device(),
            "This computer",
            true,
            true,
            vec![wake_route(1, LAN)],
            OwnerSession::default(),
        );
        link.put_device(
            laptop(),
            "Laptop",
            true,
            display,
            vec![wake_route(2, LAN)],
            OwnerSession::default(),
        );
        link
    }

    /// A device of the person's says of itself what it says now, at a revision newer than its last.
    fn put_device(
        &self,
        device: DeviceId,
        label: &str,
        online: bool,
        display: bool,
        routes: Vec<WakeRoute>,
        session: OwnerSession,
    ) {
        if online {
            self.online
                .lock()
                .expect("the online devices")
                .insert(device.clone());
        }
        let mut fabric = self.fabric.lock().expect("the fabric");
        fabric.counter = fabric.counter.saturating_add(1);
        let stamp = FabricStamp {
            counter: fabric.counter,
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
                wake_routes: routes,
                session,
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
        fabric
            .converged
            .upsert(snapshot)
            .expect("the snapshot is valid");
    }

    /// The session the person's laptop says it has now.
    fn laptop_session(&self, session: OwnerSession) {
        self.put_device(
            laptop(),
            "Laptop",
            true,
            true,
            vec![wake_route(2, LAN)],
            session,
        );
    }

    /// What this device's session last said of itself.
    fn published(&self) -> Option<OwnerSession> {
        self.fabric.lock().expect("the fabric").published
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
        self.online.lock().expect("the online devices").clone()
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

    fn share_wake_routes(&self, routes: Vec<WakeRoute>) {
        *self.routes.lock().expect("the routes") = routes;
    }

    fn require_media(&self) -> Result<(), String> {
        (*self.media.lock().expect("the media switch"))
            .then_some(())
            .ok_or_else(|| "media continuity is off".to_owned())
    }

    fn publish_owner_session(&self, session: OwnerSession) {
        self.fabric.lock().expect("the fabric").published = Some(session);
    }
}

/// A password the session can check, so it locks when the person's other devices do.
struct Sesame;

impl tilo_session::unlock::PasswordVerifier for Sesame {
    fn verify(&self, _user: &str, password: &str) -> tilo_session::unlock::Verdict {
        if password == "sesame" {
            tilo_session::unlock::Verdict::Unlocks
        } else {
            tilo_session::unlock::Verdict::Wrong
        }
    }
}

/// An action host that keeps what it was asked to run.
#[derive(Default)]
struct Host {
    invoked: RefCell<Vec<(RemoteActionTarget, Arguments)>>,
}

impl AppActions for Host {
    fn published(&self, _: &str) -> Vec<PublishedActions> {
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

/// The player the session hosts: what plays here, published as the audio service publishes it, and
/// whether that reading moved since the session last read it.
struct Player {
    playing: Rc<RefCell<Option<serde_json::Value>>>,
    moved: Rc<Cell<bool>>,
}

impl tilo_session::Service for Player {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![tilo_actions::ResourceSpec::new(
            tilo_session::MEDIA,
            "media",
            "Player",
        )]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        if resource != tilo_session::MEDIA {
            return None;
        }
        self.playing.borrow().clone()
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Arguments,
    ) -> Result<tilo_service::Started, DoorError> {
        // It offers no actions: the tests move what plays between devices through the fabric, never
        // through its own transport.
        Err(DoorError::Unknown(tilo_actions::ActionAddress::new(
            resource, action,
        )))
    }

    fn poll(&mut self) -> bool {
        self.moved.replace(false)
    }
}

/// The session and the device host behind its panel, which the test hands what plays.
struct Harness {
    session: Session,
    /// What the player publishes, as the fixture hands it what plays.
    playing: Rc<RefCell<Option<serde_json::Value>>>,
    /// Whether that moved since the session read it.
    moved: Rc<Cell<bool>>,
}

fn session(link: &FakeLink, host: Rc<Host>) -> Harness {
    session_with(link, host, None, None)
}

/// The session, with what it sends its wake packets through and which interfaces it offers to be
/// woken on.
fn session_with(
    link: &FakeLink,
    host: Rc<Host>,
    packets: Option<Arc<Packets>>,
    interfaces: Option<Arc<FixedInterfaces>>,
) -> Harness {
    let fabric: SharedLink = Arc::new(link.clone());
    let playing: Rc<RefCell<Option<serde_json::Value>>> = Rc::default();
    let moved: Rc<Cell<bool>> = Rc::default();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: vec![Box::new(Player {
                playing: Rc::clone(&playing),
                moved: Rc::clone(&moved),
            })],
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
            wake_seams: WakeSeams {
                packets: packets.map(|packets| packets as Arc<dyn MagicPackets>),
                interfaces: interfaces.map(|interfaces| interfaces as Arc<dyn WakeInterfaces>),
            },
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
            surfaces: vec![
                Surface::new(
                    "lock",
                    tilo_test_env::sibling_binary("tilo-lock-screen", "tilo-lock-screen"),
                )
                .person("ada")
                .lock(),
            ],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    Harness {
        session,
        playing,
        moved,
    }
}

impl Harness {
    /// The person's devices as the session publishes them, once it has: the reading the Quick
    /// Settings app and everything else read.
    fn reading(&self) -> Option<DevicesReading> {
        self.session
            .read(tilo_session::DEVICES_RESOURCE)
            .map(|published| serde_json::from_value(published).expect("the devices reading parses"))
    }

    /// The Devices section's rows, as the published reading lists them.
    fn rows(&self) -> Vec<tilo_quick_settings::hosted::HostedRow> {
        self.reading()
            .map(|reading| reading.rows())
            .unwrap_or_default()
    }

    /// What plays here: a browser stream, playing at 1:12, as the audio service publishes it.
    fn playing(&self) {
        *self.playing.borrow_mut() = Some(
            serde_json::to_value(what_plays())
                .expect("what plays, as the audio service publishes it"),
        );
        self.moved.set(true);
    }

    fn step(&mut self) {
        self.session.step(Duration::from_millis(1)).expect("step");
    }

    /// Press a row of the person's devices, as the panel does: the session's published verb,
    /// through the door every caller uses.
    fn press(&mut self, row: &str, part: RowPart) {
        self.session
            .invoke(
                tilo_session::DEVICES_RESOURCE,
                "press",
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
    harness.playing();
    for _ in 0..40 {
        harness.step();
        // The rows come before what the player published is read, so the wait is for the row that
        // answers with it.
        if harness
            .rows()
            .iter()
            .any(|row| row.key == "laptop" && row.actions != HostedActions::None)
        {
            break;
        }
    }
    let reading = harness
        .reading()
        .expect("the session publishes its devices");
    let rows = reading.rows();
    assert_eq!(
        rows.iter()
            .skip(1)
            .map(|row| (row.key.as_str(), row.title.as_str(), row.detail.as_str()))
            .collect::<Vec<_>>(),
        [("laptop", "Laptop", "Online")],
        "one row per device other than this one, under the sound it can take"
    );
    assert!(
        rows[0].key.starts_with(SOUND_KEY),
        "the sound this device can send comes first: {:?}",
        rows[0]
    );
    assert_eq!(
        rows[1].actions,
        HostedActions::Select("Play Clair de lune here".into()),
        "the row says what pressing it does"
    );
    assert_eq!(reading.count(), 2, "this one counts too");
    assert_eq!(
        reading
            .targets()
            .iter()
            .map(|target| (target.id.as_str(), target.online, target.local))
            .collect::<Vec<_>>(),
        [("this-pc", true, true), ("laptop", true, false)],
        "this device is listed too, and says it is this one"
    );
}

/// Step the session until `done` holds: what the person's other devices say is followed on the pass
/// that brings it, and the screen the lock shows comes up and leaves on its own clock.
#[track_caller]
fn until(harness: &mut Harness, mut done: impl FnMut(&mut Harness) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !done(harness) {
        assert!(
            Instant::now() < deadline,
            "the session did not follow what the other devices said"
        );
        harness
            .session
            .step(Duration::from_millis(50))
            .expect("step");
    }
}

/// The person's session on their other devices: locking the laptop locks this one, unlocking it
/// again undoes that where the person was here, and this device says its own session to theirs
/// while doing so.
#[test]
fn the_person_s_lock_follows_their_other_devices() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined_at_home(true);
    let mut harness = session(&link, Rc::new(Host::default()));
    // The laptop is locked a moment after the person last touched this one: this device has not
    // been used since, so the lock is followed. The time is the laptop's own clock, ahead of this
    // device's by a moment.
    link.laptop_session(OwnerSession {
        locked: true,
        changed_unix_ms: tilo_clock::unix_now_ms().saturating_add(1),
        active_unix_ms: 0,
    });
    // The device's own state reaches the fabric on a pass of its own: the wait takes in the
    // publication too, rather than reading what it said before.
    until(&mut harness, |harness| {
        harness.session.compositor().locked()
            && harness.session.surfaces().running("lock")
            && link
                .published()
                .is_some_and(|published| published.locked && published.changed_unix_ms == 0)
    });
    assert!(
        harness.session.compositor().locked(),
        "the laptop's lock locks this device"
    );
    let published = link.published().expect("the session says its own state");
    assert!(published.locked, "this device says it is locked");
    assert_eq!(
        published.changed_unix_ms, 0,
        "a followed lock is not the person's here, and does not spread further"
    );

    // Unlocking the laptop undoes the lock this device followed, because the person was here when
    // it locked.
    link.laptop_session(OwnerSession {
        locked: false,
        changed_unix_ms: tilo_clock::unix_now_ms().saturating_add(2),
        active_unix_ms: 0,
    });
    until(&mut harness, |harness| {
        !harness.session.compositor().locked()
            && !harness.session.surfaces().running("lock")
            && link
                .published()
                .is_some_and(|published| !published.locked && published.changed_unix_ms != 0)
    });
    assert!(
        !harness.session.compositor().locked(),
        "the laptop's unlock unlocks this device again"
    );
    let published = link.published().expect("the session says its own state");
    assert!(!published.locked);
    assert_ne!(
        published.changed_unix_ms, 0,
        "the person unlocked here, and their other devices are told"
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
    for _ in 0..4 {
        harness.step();
    }
    harness.press("laptop", RowPart::Row);

    assert!(link.sent().is_empty(), "nothing plays: nothing is sent");
    let workspace = harness.session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .unwrap_or_default()
    });
    assert_eq!(
        host.invoked
            .borrow()
            .first()
            .map(|(target, _)| target.clone()),
        Some(application_action_target(
            &workspace,
            "so.tilo.settings",
            "settings.section:devices_sync",
            "open"
        )),
        "a device without a screen is managed on the devices page"
    );

    // With media continuity turned off, what plays here stays here: the press opens the device's
    // settings instead.
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

    // A device that shows has its display placed beside this one's, so its press opens that page.
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
    // The steps this test takes change nothing, the bar's own clock included: a minute turning over
    // on it is not the frame it checks.
    let _clock = tilo_test_env::held_wall_clock();
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
    // As above: the bar's clock keeps still while the frames are compared.
    let _clock = tilo_test_env::held_wall_clock();
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

#[test]
fn a_sleeping_device_is_woken_by_the_device_sharing_its_network() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(false);
    link.joined_sleeping_device(desk(), "Desk", 0x0f, LAN);
    let packets = Arc::new(Packets::default());
    let interfaces = Arc::new(FixedInterfaces(vec![wake_route(0x03, LAN)]));
    let mut harness = session_with(
        &link,
        Rc::new(Host::default()),
        Some(Arc::clone(&packets)),
        Some(interfaces),
    );
    for _ in 0..40 {
        harness.step();
        if harness.rows().iter().any(|row| row.key == "desk") {
            break;
        }
    }
    let rows = harness.rows();
    let row = rows
        .iter()
        .find(|row| row.key == "desk")
        .expect("the sleeping device's row");
    assert_eq!(row.detail, "Offline");
    assert_eq!(
        row.actions,
        HostedActions::Select("Wake".into()),
        "a device that reports a way to be woken says what its press does"
    );
    assert_eq!(
        link.shared_routes(),
        vec![wake_route(0x03, LAN)],
        "this machine offers its own interfaces to the person's devices"
    );

    harness.press("desk", RowPart::Action);
    let sent = packets.taken();
    assert_eq!(sent.len(), 1, "one packet, not two: {sent:?}");
    let (packet, on) = sent.first().expect("the packet");
    assert_eq!(on, &subnet(LAN), "on the network the sleeping device is on");
    assert_eq!(packet.len(), 102);
    assert_eq!(packet.get(..6), Some(&[0xFF; 6][..]), "six bytes of ones");
    assert!(
        packet
            .get(6..)
            .expect("the rest")
            .chunks(6)
            .all(|chunk| chunk == [0x52, 0x54, 0, 0, 0, 0x0f]),
        "then the device's address, sixteen times"
    );
    assert!(
        link.sent().iter().any(|(target, action)| target == &desk()
            && matches!(action, PersonalFabricAction::WakeProbe { .. })),
        "and the device is asked whether it came back: {:?}",
        link.sent()
    );
}

#[test]
fn a_device_on_the_sleeping_device_s_network_is_asked_when_this_one_is_away() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(false);
    link.joined_awake_device(server(), "Server", 0x11, LAN);
    link.joined_sleeping_device(desk(), "Desk", 0x0f, LAN);
    // This machine is on another network, so it shares nothing to broadcast on: the device that
    // does sends the packet.
    let packets = Arc::new(Packets::default());
    let interfaces = Arc::new(FixedInterfaces(vec![wake_route(0x03, AWAY)]));
    let mut harness = session_with(
        &link,
        Rc::new(Host::default()),
        Some(Arc::clone(&packets)),
        Some(interfaces),
    );
    harness.press("desk", RowPart::Row);

    assert!(
        packets.taken().is_empty(),
        "nothing this machine sends could reach it"
    );
    let sent = link.sent();
    assert_eq!(
        sent.len(),
        1,
        "one request, to the device that can: {sent:?}"
    );
    let (target, action) = sent.first().expect("the request");
    assert_eq!(target, &server());
    assert_eq!(action, &PersonalFabricAction::WakeDevice { target: desk() });
}

#[test]
fn a_sleeping_device_that_reports_no_way_to_be_woken_opens_its_settings() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(false);
    link.joined_device(desk(), "Desk", false, false);
    let packets = Arc::new(Packets::default());
    let interfaces = Arc::new(FixedInterfaces(vec![wake_route(0x03, LAN)]));
    let host = Rc::new(Host::default());
    let mut harness = session_with(
        &link,
        Rc::clone(&host),
        Some(Arc::clone(&packets)),
        Some(interfaces),
    );
    for _ in 0..40 {
        harness.step();
        if harness.rows().iter().any(|row| row.key == "desk") {
            break;
        }
    }
    let rows = harness.rows();
    let row = rows
        .iter()
        .find(|row| row.key == "desk")
        .expect("the sleeping device's row");
    assert_eq!(
        row.actions,
        HostedActions::None,
        "no wake route of its own: nothing to offer"
    );

    harness.press("desk", RowPart::Row);
    assert!(packets.taken().is_empty());
    assert!(link.sent().is_empty());
    assert!(
        !host.invoked.borrow().is_empty(),
        "its settings open instead"
    );
}

/// Step until the session publishes the Devices section's rows: the reading is the session's, so it
/// has its rows with no panel open.
fn open_devices(harness: &mut Harness) {
    for _ in 0..40 {
        harness.step();
        if harness.rows().len() > 1 {
            return;
        }
    }
    panic!("the Devices section has its rows");
}

/// The row `key` names, as the session publishes it.
fn row(harness: &Harness, key: &str) -> tilo_quick_settings::hosted::HostedRow {
    harness
        .rows()
        .into_iter()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("the row {key} is in the section"))
}

/// The one row whose key starts with `prefix` .
fn the_row(harness: &Harness, prefix: &str) -> tilo_quick_settings::hosted::HostedRow {
    let rows = harness.rows();
    let found = rows
        .iter()
        .filter(|row| row.key.starts_with(prefix))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1, "one row for {prefix}: {rows:?}");
    found.into_iter().next().expect("the row")
}

/// Pick the sound this device plays and send it to the laptop, answering the offers the route and
/// the ask need. Returns the route's session.
fn sent_our_sound(harness: &mut Harness, link: &FakeLink) -> String {
    harness.playing();
    open_devices(harness);
    let sound = the_row(harness, SOUND_KEY);
    assert_eq!(sound.title, "All system audio");
    assert_eq!(sound.detail, "Choose a device below");
    assert!(!sound.selected, "nothing is picked to begin with");
    harness.press(&sound.key, RowPart::Row);
    assert!(the_row(harness, SOUND_KEY).selected, "the press picks it");
    assert!(link.sent().is_empty(), "picking a sound sends nothing");
    assert_eq!(
        row(harness, "laptop").actions,
        HostedActions::Select("Send".into()),
        "the device row says what a press does now"
    );
    harness.press("laptop", RowPart::Action);
    the_row(harness, ROUTE_KEY).key
}

#[test]
fn the_sound_of_this_device_travels_to_another_and_its_route_is_listed() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(true);
    let mut harness = session(&link, Rc::new(Host::default()));
    let route = sent_our_sound(&mut harness, &link);

    let sent = link.sent();
    assert_eq!(sent.len(), 1, "one offer, not two: {sent:?}");
    let (target, action) = sent.first().expect("the offer");
    assert_eq!(target, &laptop(), "the device pressed");
    let PersonalFabricAction::RemoteAudioOffer {
        session_id,
        label,
        sample_rate_hz,
        channels,
        frame_samples,
        ..
    } = action
    else {
        panic!("not an offer of sound: {action:?}");
    };
    assert_eq!(label, "All system audio");
    assert!(*sample_rate_hz > 0 && *channels > 0 && *frame_samples > 0);
    assert_eq!(
        route,
        format!("{ROUTE_KEY}{}", hex::encode(session_id)),
        "the route's row is the session the service made"
    );
    let waiting = row(&harness, &route);
    assert_eq!(waiting.title, "All system audio");
    assert_eq!(waiting.detail, "Waiting for Laptop to allow it");
    assert_eq!(waiting.actions, HostedActions::Select("Stop".into()));

    // The picker's entry was spent on the offer: the sound the person can send again is a fresh
    // one, and none is picked.
    assert!(
        !harness
            .rows()
            .iter()
            .any(|row| row.key.starts_with(SOUND_KEY) && row.selected),
        "no sound is picked after sending one"
    );
    assert_eq!(
        row(&harness, "laptop").actions,
        HostedActions::Select("Play Clair de lune here".into()),
        "with no sound picked, a press plays what plays here again"
    );
}

#[test]
fn a_route_is_stopped_from_its_row() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(true);
    let mut harness = session(&link, Rc::new(Host::default()));
    let route = sent_our_sound(&mut harness, &link);
    harness.press(&route, RowPart::Action);

    let sent = link.sent();
    assert_eq!(sent.len(), 2, "the offer and its cancellation: {sent:?}");
    let (target, action) = sent.last().expect("the cancellation");
    assert_eq!(target, &laptop());
    assert!(
        matches!(action, PersonalFabricAction::RemoteAudioCancel { .. }),
        "a route nobody answered is called off: {action:?}"
    );
    assert!(
        !harness.rows().iter().any(|row| row.key == route),
        "the stopped route is no longer listed"
    );
}

#[test]
fn a_device_asking_for_this_sound_is_declined_from_its_row() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(true);
    let mut harness = session(&link, Rc::new(Host::default()));
    open_devices(&mut harness);

    // The laptop asks to play what plays here.
    let session_id = [0x2a; 16];
    let offer = PersonalFabricActionEnvelope {
        request_id: [0x11; 16],
        source: laptop(),
        target: this_device(),
        issued_unix_ms: ui::time::unix_now_ms(),
        action: PersonalFabricAction::RemoteAudioOffer {
            session_id,
            protocol_version: REMOTE_AUDIO_PROTOCOL_VERSION,
            label: "All system audio".into(),
            sample_rate_hz: REMOTE_AUDIO_SAMPLE_RATE_HZ,
            channels: REMOTE_AUDIO_CHANNELS,
            frame_samples: REMOTE_AUDIO_FRAME_SAMPLES,
        },
    };
    let as_link: &dyn FabricLink = &link;
    assert!(tilo_speaker_service::remote_audio::handle_action(
        as_link, &offer
    ));
    for _ in 0..4 {
        harness.step();
    }

    let ask = the_row(&harness, ASK_KEY);
    assert_eq!(ask.title, "Play All system audio here");
    assert_eq!(ask.detail, "Laptop asked for this");
    assert_eq!(
        ask.actions,
        HostedActions::Pair {
            first: (
                ui::icons::Icon::Check,
                "Allow this remote audio once".into()
            ),
            second: (ui::icons::Icon::X, "Decline this remote audio".into()),
        },
        "the person allows it once or declines it"
    );
    harness.press(&ask.key, RowPart::Decline);
    let sent = link.sent();
    assert_eq!(sent.len(), 1, "one answer: {sent:?}");
    let (target, action) = sent.first().expect("the answer");
    assert_eq!(target, &laptop());
    assert_eq!(
        action,
        &PersonalFabricAction::RemoteAudioDecision {
            session_id,
            accepted: false,
            authorization: None,
        }
    );
    assert!(
        !harness
            .rows()
            .iter()
            .any(|row| row.key.starts_with(ASK_KEY)),
        "an answered ask is gone"
    );
}

/// What the audio service publishes as the players: a browser stream,
/// playing at 1:12, which another of the person's devices can continue.
fn what_plays() -> tilo_audio_service::media::Media {
    tilo_audio_service::media::Media {
        players: vec![tilo_audio_service::media::Player {
            token: tilo_audio_service::media::player_token("org.mpris.MediaPlayer2.browser"),
            bus: "org.mpris.MediaPlayer2.browser".into(),
            identity: "Music".into(),
            title: Some("Clair de lune".into()),
            artist: Some("Claude Debussy".into()),
            playing: true,
            paused: false,
            can_play: true,
            can_pause: true,
            can_next: true,
            can_previous: true,
            art: None,
            position_ms: 72_000,
            rate: 1.0,
            duration_ms: Some(300_000),
            can_seek: true,
            url: Some("https://media.example/clair.flac".into()),
            app_id: Some("so.tilo.music".into()),
            session_id: Some("so.tilo.music:mpris:0123456789abcdef".into()),
        }],
        outputs: Vec::new(),
        problem: None,
    }
}

/// What plays reaches the desk's own widget from the same reading the panel shows, with the panel
/// never opened: the widget is a view of the audio service's players, not of the panel.
#[test]
fn what_plays_reaches_the_desk_with_the_panel_closed() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined(true);
    let mut harness = session(&link, Rc::new(Host::default()));
    for _ in 0..4 {
        harness.step();
    }
    assert!(
        !harness.session.quick_settings_open(),
        "the panel stays closed"
    );
    assert!(
        !harness
            .session
            .accessible_labels()
            .iter()
            .any(|label| label == "Open Music"),
        "nothing plays: the desk shows no song"
    );

    harness.playing();
    for _ in 0..6 {
        harness.step();
    }
    assert!(
        !harness.session.quick_settings_open(),
        "the panel was never opened"
    );
    assert!(
        harness
            .session
            .accessible_labels()
            .iter()
            .any(|label| label == "Open Music"),
        "the desk shows what plays: {:?}",
        harness.session.accessible_labels()
    );
}
