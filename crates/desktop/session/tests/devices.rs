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
use std::rc::Rc;
use std::time::{Duration, Instant};

use tilo_actions::ApiError;
use tilo_actions::remote::{RemoteActionTarget, application_action_target};
use tilo_compositor::OutputConfig;
use tilo_desktop_search::providers::AppEntry;
use tilo_session::daemon_fixture::TestDaemon;
use tilo_session::hosted::{ASK_KEY, DevicesReading, HostedActions, ROUTE_KEY, RowPart, SOUND_KEY};
use tilo_session::platform::fabric::{
    DeviceAbout, DeviceId, DeviceKind, FabricCapabilities, MediaSourceLocator,
    PersonalFabricAction, PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS,
    REMOTE_AUDIO_FRAME_SAMPLES, REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_RATE_HZ,
};
use tilo_session::surfaces::Surface;
use tilo_session::{AppActions, AppLists, Lists, Options, Session};

type Arguments = serde_json::Map<String, serde_json::Value>;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// The 32 bytes that name `device` in the record of the person's devices.
fn owner_id(device: &str) -> [u8; 32] {
    let mut raw = [0; 32];
    raw[..device.len()].copy_from_slice(device.as_bytes());
    raw
}

/// The key `device` is named by in the record of the person's devices.
fn key_of(device: &str) -> String {
    hex::encode(owner_id(device))
}

/// The other of the person's devices, online with this one.
fn laptop() -> DeviceId {
    DeviceId::new(key_of("laptop")).expect("a device id")
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
    fn invoke(&self, target: RemoteActionTarget, arguments: Arguments) -> Result<(), ApiError> {
        self.invoked.borrow_mut().push((target, arguments));
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
    ) -> Result<tilo_service::Started, ApiError> {
        Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
            resource, action,
        )))
    }

    fn poll(&mut self) -> bool {
        self.moved.replace(false)
    }
}

/// The session and the device daemon behind its panel, which the test hands what plays.
struct Harness {
    session: Session,
    /// The daemon this session runs on, and whose readings it follows.
    daemon: TestDaemon,
    /// The home whose identity the daemon serves: the session reads its own device from it.
    _home: tilo_test_env::TestHome,
    /// This device's key, the one its daemon runs under.
    local: [u8; 32],
    /// What the player publishes, as the fixture hands it what plays.
    playing: Rc<RefCell<Option<serde_json::Value>>>,
    /// Whether that moved since the session read it.
    moved: Rc<Cell<bool>>,
}

/// Keep `device` in the person's record, saying it shows when `display`.
fn keep(daemon: &TestDaemon, device: [u8; 32], label: &str, display: bool) {
    let cert = daemon.certifying(device, label);
    daemon.keeping(
        &cert,
        &DeviceAbout {
            label: label.into(),
            kind: DeviceKind::Laptop,
            capabilities: FabricCapabilities {
                display,
                audio_output: true,
                ..FabricCapabilities::default()
            },
        },
        &[],
    );
}

/// Say `device` has no sound output, as a device that cannot take what plays here.
fn keep_silent(daemon: &TestDaemon, device: [u8; 32], label: &str, display: bool) {
    let cert = daemon.certifying(device, label);
    daemon.keeping(
        &cert,
        &DeviceAbout {
            label: label.into(),
            kind: DeviceKind::Laptop,
            capabilities: FabricCapabilities {
                display,
                audio_output: false,
                ..FabricCapabilities::default()
            },
        },
        &[],
    );
}

/// A session on a daemon holding the person's own workspace, with this computer and `devices` in
/// their record: each a name, a label, whether it is online and whether it shows.
fn session(devices: &[(&str, &str, bool, bool)], host: Rc<Host>) -> Harness {
    session_with(devices, host, true, &[])
}

/// [`session`], with media continuity on or off as the person set it on this device, and the
/// devices its daemon hears on this machine's network.
fn session_with(
    devices: &[(&str, &str, bool, bool)],
    host: Rc<Host>,
    media: bool,
    nearby: &[&str],
) -> Harness {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let daemon = TestDaemon::start();
    daemon.serve_identity(&mut home);
    daemon.found_person_workspace();
    let local = daemon.certificate().device_public_key;
    let mut online = vec![local];
    keep(&daemon, local, "This computer", true);
    for (device, label, on, display) in devices {
        let device = owner_id(device);
        keep(&daemon, device, label, *display);
        if *on {
            online.push(device);
        }
    }
    daemon.set_online(online);
    daemon.set_nearby(nearby.iter().map(|device| owner_id(device)));
    if !media {
        settings_store::update_section(
            "continuity",
            serde_json::json!({ "enabled": false })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        )
        .expect("the device's settings are written");
    }
    let person = daemon.certificate().user_public_key;
    let playing: Rc<RefCell<Option<serde_json::Value>>> = Rc::default();
    let moved: Rc<Cell<bool>> = Rc::default();
    let session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            services: vec![Box::new(Player {
                playing: Rc::clone(&playing),
                moved: Rc::clone(&moved),
            })],
            lists: Lists::fixed(AppLists {
                applications: vec![AppEntry {
                    id: "so.tilo.settings".into(),
                    title: "Settings".into(),
                    category: None,
                }],
                ..AppLists::default()
            }),
            apps: Some(host),
            person: hex::encode(person),
            state: Some(home.state().to_path_buf()),
            shell_link: Some(daemon.shell_link()),
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
            surfaces: vec![
                Surface::new(
                    "lock",
                    tilo_test_env::sibling_binary("tilo-lock-screen", "tilo-lock-screen"),
                )
                .lock(),
            ],
            ..Options::default()
        },
    );
    let mut harness = Harness {
        session,
        daemon,
        _home: home,
        local,
        playing,
        moved,
    };
    // The send a press makes goes over the link the session asks its daemon for as it starts.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !harness.daemon.speakers_attached() {
        assert!(
            Instant::now() < deadline,
            "the session never took its daemon's speakers link"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    harness
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
    fn rows(&self) -> Vec<tilo_session::hosted::HostedRow> {
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
    /// through the API every caller uses.
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

    /// What the session asked its daemon to hand the person's devices, each envelope with the
    /// device it is for, once the daemon took the first.
    fn sent(&mut self) -> Vec<(DeviceId, PersonalFabricAction)> {
        self.sent_until(1)
    }

    /// [`Self::sent`], once the daemon took `n` of them: a press that adds to an envelope already
    /// sent waits for its own, and a send that never comes fails the test with what it saw.
    fn sent_until(&mut self, n: usize) -> Vec<(DeviceId, PersonalFabricAction)> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let sent: Vec<(DeviceId, PersonalFabricAction)> = self
                .daemon
                .sent_speakers()
                .into_iter()
                .filter_map(|(peer, envelope)| {
                    let envelope: PersonalFabricActionEnvelope =
                        postcard::from_bytes(&envelope).ok()?;
                    Some((DeviceId::new(hex::encode(peer)).ok()?, envelope.action))
                })
                .collect();
            if sent.len() >= n || Instant::now() > deadline {
                return sent;
            }
            self.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Another of the person's devices says `envelope` here, as its session hands it to the link.
    fn hearing(&mut self, envelope: &PersonalFabricActionEnvelope) {
        let peer: [u8; 32] = hex::decode(envelope.source.as_str())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .expect("the sender's key");
        self.daemon
            .hearing(tilo_session_client::speakers::Frame::Heard {
                peer,
                envelope: postcard::to_allocvec(envelope).expect("the envelope encodes"),
                received_us: 0,
            });
        for _ in 0..4 {
            self.step();
        }
    }
}

#[test]
fn the_person_s_devices_become_rows_and_where_an_accessory_can_move() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
    harness.playing();
    for _ in 0..40 {
        harness.step();
        // The rows come before what the player published is read, so the wait is for the row that
        // answers with it.
        if harness
            .rows()
            .iter()
            .any(|row| row.key == key_of("laptop") && row.actions != HostedActions::None)
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
            .map(|row| (row.key.clone(), row.title.clone(), row.detail.clone()))
            .collect::<Vec<_>>(),
        [(key_of("laptop"), "Laptop".to_owned(), "Online".to_owned())],
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
        [
            (hex::encode(harness.local).as_str(), true, true),
            (key_of("laptop").as_str(), true, false),
        ],
        "this device is listed too, and says it is this one"
    );
}

/// A device of the person's heard on this machine's local network says so on its row: the two reach
/// each other there without a server.
#[test]
fn a_device_on_the_local_network_is_listed_as_nearby() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session_with(
        &[("laptop", "Laptop", true, false)],
        Rc::new(Host::default()),
        true,
        &["laptop"],
    );
    // The reading the session follows moves when the daemon says so again.
    harness.daemon.set_nearby([owner_id("laptop")]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        harness.step();
        if harness
            .rows()
            .iter()
            .any(|row| row.key == key_of("laptop") && row.detail == "Nearby")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the laptop's row says it is on this network: {:?}",
            harness.rows()
        );
    }
    let reading = harness
        .reading()
        .expect("the session publishes its devices");
    assert_eq!(
        reading
            .rows()
            .iter()
            .filter(|row| !row.key.starts_with(SOUND_KEY))
            .map(|row| (row.key.clone(), row.detail.clone()))
            .collect::<Vec<_>>(),
        [(key_of("laptop"), "Nearby".to_owned())],
        "the laptop's row says it is on this network"
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

#[test]
fn pressing_another_device_sends_it_what_plays_here() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
    harness.playing();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        harness.step();
        if row(&harness, &key_of("laptop")).actions
            == HostedActions::Select("Play Clair de lune here".into())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the laptop's row offers what plays here: {:#?}",
            row(&harness, &key_of("laptop"))
        );
    }
    let started = Instant::now();
    harness.press(&key_of("laptop"), RowPart::Action);
    let pressed = started.elapsed();
    let sent = harness.sent();
    let took = started.elapsed();
    println!(
        "the press was carried out in {pressed:?}, and its frame reached the daemon in {took:?}"
    );
    assert!(
        took < Duration::from_secs(1),
        "a person's press reaches the daemon well under a second, and took {took:?}"
    );
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
fn a_device_that_cannot_play_is_not_sent_what_plays_here() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
    harness.playing();
    // The row answers with what plays here only while the device can play it.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        harness.step();
        if row(&harness, &key_of("laptop")).actions
            == HostedActions::Select("Play Clair de lune here".into())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the laptop's row offers what plays here: {:#?}",
            row(&harness, &key_of("laptop"))
        );
    }
    keep_silent(&harness.daemon, owner_id("laptop"), "Laptop", true);

    // A device with no sound output is not offered the hand-over, and pressing it sends nothing:
    // it opens that device's settings, as a press on a device with nothing playing does.
    until(&mut harness, |harness| {
        row(harness, &key_of("laptop")).actions
            != HostedActions::Select("Play Clair de lune here".into())
    });
    harness.press(&key_of("laptop"), RowPart::Row);
    assert!(
        harness.sent().is_empty(),
        "a device that cannot play is not sent what plays here: {:?}",
        harness.sent()
    );
}

#[test]
fn pressing_a_device_with_nothing_playing_opens_its_settings() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let host = Rc::new(Host::default());
    let mut harness = session(&[("laptop", "Laptop", true, false)], Rc::clone(&host));
    for _ in 0..4 {
        harness.step();
    }
    harness.press(&key_of("laptop"), RowPart::Row);

    assert!(harness.sent().is_empty(), "nothing plays: nothing is sent");
    let workspace = harness.session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .unwrap_or_default()
    });
    assert_eq!(
        host.invoked.borrow().first().cloned(),
        Some((
            application_action_target(
                &workspace,
                "so.tilo.settings",
                "tl:so.tilo.settings",
                "open_pane"
            ),
            serde_json::Map::from_iter([("pane".to_owned(), serde_json::json!("devices_sync"))])
        )),
        "a device without a screen is managed on the devices page, through the page's own row"
    );

    // With media continuity turned off, what plays here stays here: the press opens the device's
    // settings instead.
    let host = Rc::new(Host::default());
    let mut harness = session_with(
        &[("laptop", "Laptop", true, false)],
        Rc::clone(&host),
        false,
        &[],
    );
    harness.playing();
    for _ in 0..4 {
        harness.step();
    }
    harness.press(&key_of("laptop"), RowPart::Action);
    assert!(harness.sent().is_empty(), "media continuity is off");
    assert!(
        !host.invoked.borrow().is_empty(),
        "its settings page opens instead"
    );

    // A device that shows has its display placed beside this one's, so its press opens that page.
    let host = Rc::new(Host::default());
    let mut harness = session(&[("laptop", "Laptop", true, true)], Rc::clone(&host));
    for _ in 0..2 {
        harness.step();
    }
    harness.press(&key_of("laptop"), RowPart::Row);
    assert!(harness.sent().is_empty(), "nothing plays: nothing is sent");
    let workspace = harness.session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .unwrap_or_default()
    });
    let displays = application_action_target(
        &workspace,
        "so.tilo.settings",
        "tl:so.tilo.settings",
        "open_pane",
    );
    assert!(
        host.invoked.borrow().iter().any(|(target, arguments)| {
            *target == displays && arguments.get("pane") == Some(&serde_json::json!("displays"))
        }),
        "a device that shows opens the displays page, through the page's own row: {:?}",
        host.invoked.borrow()
    );
}

#[test]
fn this_device_s_row_does_nothing() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // The steps this test takes change nothing, the bar's own clock included: a minute turning over
    // on it is not the frame it checks.
    let _clock = tilo_test_env::held_wall_clock();
    let host = Rc::new(Host::default());
    let mut harness = session(&[("laptop", "Laptop", true, true)], Rc::clone(&host));
    harness.playing();
    for _ in 0..4 {
        harness.step();
    }
    let this_pc = hex::encode(harness.local);
    harness.press(&this_pc, RowPart::Row);
    assert!(harness.sent().is_empty());
    assert!(host.invoked.borrow().is_empty());
}

#[test]
fn a_row_a_device_left_behind_is_a_press_on_nothing() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // As above: the bar's clock keeps still while the frames are compared.
    let _clock = tilo_test_env::held_wall_clock();
    let host = Rc::new(Host::default());
    let mut harness = session(&[("laptop", "Laptop", true, true)], Rc::clone(&host));
    harness.playing();
    for _ in 0..4 {
        harness.step();
    }
    harness.press("phone", RowPart::Row);
    assert!(harness.sent().is_empty());
    assert!(host.invoked.borrow().is_empty());
}

#[test]
fn a_sleeping_device_that_reports_no_way_to_be_woken_opens_its_settings() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let host = Rc::new(Host::default());
    let mut harness = session(
        &[
            ("laptop", "Laptop", true, false),
            ("session", "Session", false, false),
        ],
        Rc::clone(&host),
    );
    for _ in 0..40 {
        harness.step();
        if harness
            .rows()
            .iter()
            .any(|row| row.key == key_of("session"))
        {
            break;
        }
    }
    let rows = harness.rows();
    let row = rows
        .iter()
        .find(|row| row.key == key_of("session"))
        .expect("the sleeping device's row");
    assert_eq!(
        row.actions,
        HostedActions::None,
        "nothing says it wakes: nothing to offer"
    );

    harness.press(&key_of("session"), RowPart::Row);
    assert!(harness.sent().is_empty());
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
fn row(harness: &Harness, key: &str) -> tilo_session::hosted::HostedRow {
    harness
        .rows()
        .into_iter()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("the row {key} is in the section"))
}

/// The one row whose key starts with `prefix` .
fn the_row(harness: &Harness, prefix: &str) -> tilo_session::hosted::HostedRow {
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
fn sent_our_sound(harness: &mut Harness) -> String {
    harness.playing();
    open_devices(harness);
    let sound = the_row(harness, SOUND_KEY);
    assert_eq!(sound.title, "All system audio");
    assert_eq!(sound.detail, "Choose a device below");
    assert!(!sound.selected, "nothing is picked to begin with");
    harness.press(&sound.key, RowPart::Row);
    assert!(the_row(harness, SOUND_KEY).selected, "the press picks it");
    assert!(harness.sent().is_empty(), "picking a sound sends nothing");
    assert_eq!(
        row(harness, &key_of("laptop")).actions,
        HostedActions::Select("Send".into()),
        "the device row says what a press does now"
    );
    harness.press(&key_of("laptop"), RowPart::Action);
    the_row(harness, ROUTE_KEY).key
}

#[test]
fn the_sound_of_this_device_travels_to_another_and_its_route_is_listed() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
    let route = sent_our_sound(&mut harness);

    let sent = harness.sent();
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
        row(&harness, &key_of("laptop")).actions,
        HostedActions::Select("Play Clair de lune here".into()),
        "with no sound picked, a press plays what plays here again"
    );
}

#[test]
fn a_route_is_stopped_from_its_row() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
    let route = sent_our_sound(&mut harness);
    harness.press(&route, RowPart::Action);

    let sent = harness.sent_until(2);
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
    open_devices(&mut harness);

    // The laptop asks to play what plays here.
    let session_id = [0x2a; 16];
    let offer = PersonalFabricActionEnvelope {
        request_id: [0x11; 16],
        source: laptop(),
        target: DeviceId::new(hex::encode(harness.local)).expect("a device id"),
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
    harness.hearing(&offer);

    let ask = the_row(&harness, ASK_KEY);
    assert_eq!(ask.title, "Play All system audio here");
    assert_eq!(ask.detail, "Laptop asked for this");
    assert_eq!(
        ask.actions,
        HostedActions::Pair {
            first: (
                tilo_ui_base::icons::Icon::Check,
                "Allow this remote audio once".into()
            ),
            second: (
                tilo_ui_base::icons::Icon::X,
                "Decline this remote audio".into()
            ),
        },
        "the person allows it once or declines it"
    );
    harness.press(&ask.key, RowPart::Decline);
    let sent = harness.sent();
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

/// What plays reaches the shell's own widget from the same reading the panel shows, with the panel
/// never opened: the widget is a view of the audio service's players, not of the panel.
#[test]
fn what_plays_reaches_the_shell_with_the_panel_closed() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
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
            .any(|label| label == "Clair de lune"),
        "nothing plays: the session shows no song"
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
            .any(|label| label == "Clair de lune"),
        "the session shows what plays: {:?}",
        harness.session.accessible_labels()
    );
}

/// A shell form of another language reads whether this device is the person's and the workspaces
/// their layout lists, under the keys and names the layout uses. A login that reaches no daemon
/// fails, and a session on the person's fabric stays theirs.
#[test]
fn a_view_reads_the_login_and_the_layouts_workspaces() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
    for _ in 0..4 {
        harness.step();
    }
    let reading = harness
        .reading()
        .expect("the session publishes its devices");
    assert!(
        reading.signed_in,
        "a session on the person's fabric is one of their devices"
    );
    let layout = harness.session.layout_reading();
    assert!(
        !layout.workspaces.is_empty(),
        "the layout holds a workspace"
    );
    assert_eq!(
        reading
            .workspaces
            .iter()
            .map(|workspace| (workspace.key.as_str(), workspace.name.as_str()))
            .collect::<Vec<_>>(),
        layout
            .workspaces
            .iter()
            .map(|workspace| (workspace.key.as_str(), workspace.name.as_str()))
            .collect::<Vec<_>>(),
        "the same workspaces, under the keys and names the layout uses"
    );
    // A login is this computer's daemon's, and no daemon answers here: it fails and changes
    // nothing of the reading, never a silent success.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove(tilo_api::calls::KEY_FILE_ENV);
    let ticket = serde_json::json!({"ticket": "tilo1-a-ticket"});
    harness
        .session
        .invoke(
            tilo_session::DEVICES_RESOURCE,
            "log-in",
            ticket.as_object().expect("arguments"),
        )
        .expect("the login is taken");
    until(&mut harness, |harness| {
        harness
            .reading()
            .is_some_and(|reading| !reading.refused.is_empty())
    });
    let reading = harness
        .reading()
        .expect("the session still publishes its devices");
    assert!(
        reading.signed_in,
        "a login that failed leaves the session the person's device"
    );
    assert!(
        !reading.refused.is_empty(),
        "what went wrong reads under the ticket field"
    );
}

/// A shell form of another language takes another of the person's devices back through this
/// device's daemon, which signs the revocation: with no daemon answering, the call fails and says
/// so, never a silent success.
#[test]
fn a_revocation_that_reaches_no_daemon_fails_and_says_so() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut harness = session(
        &[("laptop", "Laptop", true, true)],
        Rc::new(Host::default()),
    );
    for _ in 0..4 {
        harness.step();
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove(tilo_api::calls::KEY_FILE_ENV);
    let device = serde_json::json!({"device": "00"});
    harness
        .session
        .invoke(
            tilo_session::DEVICES_RESOURCE,
            "revoke",
            device.as_object().expect("arguments"),
        )
        .expect("the revocation is taken");
    until(&mut harness, |harness| {
        harness
            .reading()
            .is_some_and(|reading| !reading.refused.is_empty())
    });
    let reading = harness
        .reading()
        .expect("the session still publishes its devices");
    assert!(
        reading.signed_in,
        "a revocation that failed leaves the session the person's device"
    );
    assert!(
        !reading.refused.is_empty(),
        "what went wrong is the daemon's own words"
    );
}
