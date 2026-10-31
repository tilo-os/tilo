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
use tilo_device_fabric::wake::{self, MagicPackets};
use tilo_device_fabric::{
    DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
    FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, Ipv4Subnet, MediaSourceLocator,
    OwnerSession, PersonalFabricAction, WakeInterfaceKind, WakeRoute,
};
use tilo_quick_settings::device::Device;
use tilo_quick_settings::device::PlayerLink;
use tilo_quick_settings::device::testing::{Kept, Machine};
use tilo_quick_settings::hosted::HostedActions;
use tilo_quick_settings::{QsEffect, RowPart};
use tilo_session::{
    AppActions, AppLists, DeviceHost, Lists, Options, QuickSettings, Session, WakeInterfaces,
    WakeSeams,
};

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

/// This machine's interfaces, fixed so the test does not depend on the
/// machine running it.
struct FixedInterfaces(Vec<WakeRoute>);

impl WakeInterfaces for FixedInterfaces {
    fn routes(&self) -> Vec<WakeRoute> {
        self.0.clone()
    }
}

/// The fabric as a stand-in transport keeps it, and what the session sent
/// through it.
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

    /// A device of the person's, awake now, reporting that `mac` on `on`
    /// wakes it.
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

    /// A device of the person's, asleep off the network, reporting that
    /// `mac` on `on` wakes it.
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

    /// Both of the person's devices on their home network, which is what
    /// lets one follow a lock on the other.
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

    /// A device of the person's says of itself what it says now, at a
    /// revision newer than its last.
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

/// A password the session can check, so it locks when the person's other
/// devices do.
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
    session_with(link, host, None, None)
}

/// The session, with what it sends its wake packets through and which
/// interfaces it offers to be woken on.
fn session_with(
    link: &FakeLink,
    host: Rc<Host>,
    packets: Option<Arc<Packets>>,
    interfaces: Option<Arc<FixedInterfaces>>,
) -> Harness {
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
            wake_seams: WakeSeams {
                packets: packets.map(|packets| packets as Arc<dyn MagicPackets>),
                interfaces: interfaces.map(|interfaces| interfaces as Arc<dyn WakeInterfaces>),
            },
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
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

/// The person's session on their other devices: locking the laptop locks this
/// one, unlocking it again undoes that where the person was here, and this
/// device says its own session to theirs while doing so.
#[test]
fn the_person_s_lock_follows_their_other_devices() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined_at_home(true);
    let mut harness = session(&link, Rc::new(Host::default()));
    // The laptop is locked a moment after the person last touched this one:
    // this device has not been used since, so the lock is followed. The time
    // is the laptop's own clock, ahead of this device's by a moment.
    link.laptop_session(OwnerSession {
        locked: true,
        changed_unix_ms: tilo_clock::unix_now_ms().saturating_add(1),
        active_unix_ms: 0,
    });
    for _ in 0..6 {
        harness
            .session
            .step(Duration::from_millis(1100))
            .expect("step");
        if harness.session.compositor().locked() {
            break;
        }
    }
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

    // Unlocking the laptop undoes the lock this device followed, because the
    // person was here when it locked.
    link.laptop_session(OwnerSession {
        locked: false,
        changed_unix_ms: tilo_clock::unix_now_ms().saturating_add(2),
        active_unix_ms: 0,
    });
    for _ in 0..6 {
        harness
            .session
            .step(Duration::from_millis(1100))
            .expect("step");
        if !harness.session.compositor().locked() {
            break;
        }
    }
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
    harness
        .session
        .invoke(CHROME_RESOURCE, "toggle-quick-settings", &Arguments::new())
        .expect("open quick settings");
    for _ in 0..40 {
        harness.step();
        if harness
            .device()
            .state()
            .device_rows
            .iter()
            .any(|row| row.key == "desk")
        {
            break;
        }
    }
    let state = harness.device().state();
    let row = state
        .device_rows
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
    // This machine is on another network, so it shares nothing to broadcast
    // on: the device that does sends the packet.
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
    harness
        .session
        .invoke(CHROME_RESOURCE, "toggle-quick-settings", &Arguments::new())
        .expect("open quick settings");
    for _ in 0..40 {
        harness.step();
        if harness
            .device()
            .state()
            .device_rows
            .iter()
            .any(|row| row.key == "desk")
        {
            break;
        }
    }
    let state = harness.device().state();
    let row = state
        .device_rows
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
