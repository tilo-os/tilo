//! Two of a person's devices, each with its own engine, over one record: what one asks, the other
//! decides, the hardware at one edge and the virtual device at the other. The camera, microphone,
//! scanner, virtual devices and uinput pad are fakes that keep what reached them.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "a test's counts and clock stay small"
)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use os_actions::peripheral_forwarding::MAX_EVENTS_PER_SECOND;
use os_actions::{
    ForwardedGamepadEvent, GamepadButton, LinuxGamepadDevice, LinuxGamepadDeviceProvider,
    LinuxVirtualGamepadFactory,
};
use tilo_resource::{AppNamed, Id, Replica};
use tilo_workspace_service::workspaces::person_workspace::devices::{
    Devices, RemoteKind, RemoteStage, RemoteUse,
};
use tilo_workspace_service::workspaces::person_workspace::remote_use::{
    DocumentFormat, GRANT_MS, MAX_CHUNK_BYTES, Profile, VideoFormat, asking,
};

use super::engine::{
    Capture, Edge, Engine, Front, PadSource, Question, Unit, VirtualDevice, World,
};
use super::wire::{Carried, What};

const START: i64 = 1_700_000_000_000;

fn laptop() -> String {
    "22".repeat(32)
}

fn phone() -> String {
    "33".repeat(32)
}

fn calls() -> Front {
    Front {
        app: "so.tilo.calls".to_owned(),
        workspace: "workspace-1".to_owned(),
        session: "window-1".to_owned(),
        name: "Calls".to_owned(),
    }
}

fn game() -> Front {
    Front {
        app: "so.tilo.game".to_owned(),
        workspace: "workspace-1".to_owned(),
        session: "window-7".to_owned(),
        name: "Game".to_owned(),
    }
}

/// What reached the edges of one device.
#[derive(Default)]
struct Seen {
    /// What its hardware captures next.
    captured: VecDeque<Unit>,
    /// Captures running now.
    capturing: usize,
    /// Frames its virtual camera or microphone was given, whole.
    frames: Vec<Vec<u8>>,
    /// Documents its scanner inbox was given.
    documents: Vec<Vec<Vec<u8>>>,
    /// Virtual devices in place now.
    placed: usize,
    /// What its controller sends next.
    pressed: VecDeque<ForwardedGamepadEvent>,
    /// What its uinput pads emitted, and how many exist now.
    emitted: Vec<ForwardedGamepadEvent>,
    pads: usize,
}

type Shared = Arc<Mutex<Seen>>;

struct FakeEdge(Shared);

struct FakeCapture(Shared);

impl Capture for FakeCapture {
    fn take(&mut self) -> Vec<Unit> {
        self.0.lock().unwrap().captured.drain(..).collect()
    }
}

impl Drop for FakeCapture {
    fn drop(&mut self) {
        self.0.lock().unwrap().capturing -= 1;
    }
}

struct FakeVirtual(Shared);

impl VirtualDevice for FakeVirtual {
    fn frame(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.lock().unwrap().frames.push(bytes.to_vec());
        Ok(())
    }

    fn document(&mut self, pages: Vec<Vec<u8>>) -> Result<(), String> {
        self.0.lock().unwrap().documents.push(pages);
        Ok(())
    }
}

impl Drop for FakeVirtual {
    fn drop(&mut self) {
        self.0.lock().unwrap().placed -= 1;
    }
}

struct FakePad(Shared);

impl PadSource for FakePad {
    fn take(&mut self) -> Vec<ForwardedGamepadEvent> {
        self.0.lock().unwrap().pressed.drain(..).collect()
    }
}

impl Edge for FakeEdge {
    fn capture(&self, _profile: &Profile) -> Result<Box<dyn Capture>, String> {
        self.0.lock().unwrap().capturing += 1;
        Ok(Box::new(FakeCapture(self.0.clone())))
    }

    fn virtual_device(
        &self,
        _label: &str,
        _profile: &Profile,
    ) -> Result<Box<dyn VirtualDevice>, String> {
        self.0.lock().unwrap().placed += 1;
        Ok(Box::new(FakeVirtual(self.0.clone())))
    }

    fn pad(&self, _label: &str) -> Result<Box<dyn PadSource>, String> {
        Ok(Box::new(FakePad(self.0.clone())))
    }
}

/// The uinput device node, as the Linux virtual gamepad drives it.
struct Uinput(Shared);

struct UinputPad(Shared);

impl LinuxGamepadDeviceProvider for Uinput {
    type Device = UinputPad;

    fn open_fixed_gamepad(&self) -> std::io::Result<UinputPad> {
        self.0.lock().unwrap().pads += 1;
        Ok(UinputPad(self.0.clone()))
    }
}

impl LinuxGamepadDevice for UinputPad {
    fn emit(&mut self, event: ForwardedGamepadEvent) -> std::io::Result<()> {
        self.0.lock().unwrap().emitted.push(event);
        Ok(())
    }

    fn release_all(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn destroy(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().pads -= 1;
        Ok(())
    }
}

/// One device: its engine, what reached its edges, and what its session reads.
struct Device {
    engine: Engine,
    seen: Shared,
    front: Option<Front>,
    realtime: bool,
    gamepads: bool,
    online: bool,
    heard: Vec<(String, Vec<u8>)>,
    asked: Vec<Question>,
    failed: Vec<String>,
}

impl Device {
    fn new(key: String) -> Self {
        let seen = Shared::default();
        let pads = LinuxVirtualGamepadFactory::new(Uinput(seen.clone()));
        Self {
            engine: Engine::new(key, Box::new(FakeEdge(seen.clone())), Box::new(pads)),
            seen,
            front: Some(calls()),
            realtime: true,
            gamepads: true,
            online: true,
            heard: Vec::new(),
            asked: Vec::new(),
            failed: Vec::new(),
        }
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.lock().unwrap()
    }
}

/// Two of one person's devices and the record of their devices.
struct Pair {
    record: Replica<Devices, AppNamed>,
    devices: BTreeMap<String, Device>,
    now: i64,
}

impl Pair {
    fn new() -> Self {
        let mut record = Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        for key in [laptop(), phone()] {
            record
                .apply(Devices::KEEP_FACT.with((
                    key,
                    "00".to_owned(),
                    "seen".to_owned(),
                    "00".to_owned(),
                )))
                .unwrap();
        }
        let devices = [laptop(), phone()]
            .into_iter()
            .map(|key| (key.clone(), Device::new(key)))
            .collect();
        Self {
            record,
            devices,
            now: START,
        }
    }

    fn state(&self) -> Devices {
        self.record.state().unwrap()
    }

    fn device(&mut self, key: &str) -> &mut Device {
        self.devices.get_mut(key).unwrap()
    }

    fn kept(&self, id: [u8; 16]) -> RemoteUse {
        self.state()
            .uses
            .get(&Id::from_raw(hex::encode(id)))
            .cloned()
            .unwrap()
    }

    /// `local` asks for `other`'s camera, microphone or scanner for the app it has in front, or
    /// to send `other` its controller.
    fn ask(&mut self, id: [u8; 16], local: &str, other: &str, profile: &Profile) {
        let front = self.devices[local].front.clone().unwrap();
        let named = if *profile == Profile::Controller {
            (String::new(), String::new(), "Gamepad".to_owned())
        } else {
            (front.app, front.workspace, front.name)
        };
        let write = asking(
            &self.state(),
            id,
            local,
            other,
            profile,
            (&named.0, &named.1, &named.2),
            self.now,
        )
        .unwrap();
        self.record.apply(write).unwrap();
    }

    /// One pass on each device online, in turn, with what the other sent it since.
    fn pass(&mut self) {
        let keys: Vec<String> = self.devices.keys().cloned().collect();
        let online: BTreeMap<String, bool> = self
            .devices
            .iter()
            .map(|(key, device)| (key.clone(), device.online))
            .collect();
        for key in keys {
            if !online[&key] {
                continue;
            }
            let state = self.state();
            let device = self.devices.get_mut(&key).unwrap();
            let (realtime, gamepads) = (device.realtime, device.gamepads);
            let allowed = move |kind: RemoteKind| match kind {
                RemoteKind::Controller => gamepads,
                _ => realtime,
            };
            let is_online = |other: &str| online.get(other).copied().unwrap_or(false);
            let world = World {
                record: &state,
                now_ms: self.now,
                online: &is_online,
                allowed: &allowed,
                front: device.front.clone(),
            };
            let heard = std::mem::take(&mut device.heard);
            let out = device.engine.pass(&world, heard);
            device.asked.extend(out.questions);
            device
                .failed
                .extend(out.failed.into_iter().map(|(_, why)| why));
            for write in out.writes {
                self.record.apply(write).unwrap();
            }
            for (to, body) in out.sent {
                if online.get(&to).copied().unwrap_or(false) {
                    self.devices
                        .get_mut(&to)
                        .unwrap()
                        .heard
                        .push((key.clone(), body));
                }
            }
        }
    }

    /// The person at `key` answers the question they were asked.
    fn answer(&mut self, key: &str, yes: bool) {
        let state = self.state();
        let device = self.devices.get_mut(key).unwrap();
        let question = device.asked.pop().expect("the person was asked");
        let (realtime, gamepads) = (device.realtime, device.gamepads);
        let allowed = move |kind: RemoteKind| match kind {
            RemoteKind::Controller => gamepads,
            _ => realtime,
        };
        let world = World {
            record: &state,
            now_ms: self.now,
            online: &|_| true,
            allowed: &allowed,
            front: device.front.clone(),
        };
        let write = device.engine.answered(&world, &question, yes);
        self.record.apply(write).unwrap();
    }

    fn at(&mut self, ms: i64) {
        self.now = START + ms;
    }
}

fn camera() -> Profile {
    Profile::Camera {
        format: VideoFormat::Nv12,
        width: 320,
        height: 240,
        frames_per_second: 30,
    }
}

fn frame(fill: u8) -> Vec<u8> {
    vec![fill; 320 * 240 * 3 / 2]
}

/// The laptop asks for the phone's camera for the call it has in front. Only the phone's person
/// is asked, naming the app; on their yes the phone captures and the laptop's virtual camera gets
/// every frame whole, carried in pieces of at most 64 KiB. The grant lasts five minutes and is
/// renewed while the phone sends. When the call leaves the front of the laptop, the use ends and
/// both the capture and the virtual camera go.
#[test]
fn a_camera_of_another_device_is_asked_for_granted_carried_and_torn_down_on_focus_loss() {
    let (laptop, phone) = (laptop(), phone());
    let mut pair = Pair::new();
    let id = [1; 16];
    pair.ask(id, &laptop, &phone, &camera());
    pair.pass();
    assert!(
        pair.devices[&laptop].asked.is_empty(),
        "the device asking decides nothing"
    );
    let asked = &pair.devices[&phone].asked;
    assert_eq!(asked.len(), 1, "the phone's person is asked");
    assert_eq!(
        (asked[0].kept.app.as_str(), asked[0].kept.label.as_str()),
        ("so.tilo.calls", "Calls")
    );
    pair.pass();
    assert_eq!(pair.devices[&phone].asked.len(), 1, "and asked once");

    pair.at(2_000);
    pair.answer(&phone, true);
    assert_eq!(
        pair.kept(id).until_ms,
        pair.now + GRANT_MS,
        "a grant lasts five minutes"
    );
    pair.pass();
    assert_eq!(pair.devices[&phone].seen().capturing, 1);
    assert_eq!(pair.devices[&laptop].seen().placed, 1);

    pair.device(&phone)
        .seen()
        .captured
        .extend([Unit::Frame(frame(1)), Unit::Frame(frame(2))]);
    pair.pass();
    let carried: Vec<Carried> = pair.devices[&laptop]
        .heard
        .iter()
        .map(|(_, body)| Carried::decode(body).unwrap())
        .collect();
    assert_eq!(carried.len(), 4, "a 115 200-byte frame goes in two pieces");
    assert!(carried.iter().all(|carried| matches!(&carried.what,
        What::Piece(piece) if piece.payload.len() <= MAX_CHUNK_BYTES)));
    pair.pass();
    assert_eq!(pair.devices[&laptop].seen().frames, [frame(1), frame(2)]);

    // Past half its grant, the phone that sends renews it.
    pair.at(2_000 + GRANT_MS / 2 + 1_000);
    pair.pass();
    assert_eq!(
        pair.kept(id).until_ms,
        pair.now + GRANT_MS,
        "the grant is renewed while sending"
    );

    pair.device(&laptop).front = Some(game());
    pair.pass();
    assert_eq!(
        pair.kept(id).stage,
        RemoteStage::Ended,
        "the app lost focus"
    );
    pair.pass();
    assert_eq!(
        pair.devices[&laptop].seen().placed,
        0,
        "the virtual camera is gone"
    );
    assert_eq!(
        pair.devices[&phone].seen().capturing,
        0,
        "the camera stopped"
    );
}

/// A grant whose sender is gone runs out: the device that asked, still waiting after five
/// minutes, ends it and drops its virtual microphone. A device that leaves is given up at once.
#[test]
fn a_remote_microphone_ends_when_its_grant_runs_out_or_its_device_leaves() {
    let (laptop, phone) = (laptop(), phone());
    let mut pair = Pair::new();
    let microphone = Profile::usual(RemoteKind::Microphone);
    pair.ask([2; 16], &laptop, &phone, &microphone);
    pair.pass();
    pair.answer(&phone, true);
    pair.pass();
    let sound = vec![7; 1_920];
    pair.device(&phone)
        .seen()
        .captured
        .push_back(Unit::Frame(sound.clone()));
    pair.pass();
    pair.pass();
    assert_eq!(pair.devices[&laptop].seen().frames, [sound]);

    // The phone stops taking part, though it is still listed online: nobody renews.
    pair.device(&phone).online = false;
    pair.at(GRANT_MS);
    let state = pair.state();
    let world = World {
        record: &state,
        now_ms: pair.now,
        online: &|_| true,
        allowed: &|_| true,
        front: Some(calls()),
    };
    let out = pair.device(&laptop).engine.pass(&world, Vec::new());
    for write in out.writes {
        pair.record.apply(write).unwrap();
    }
    assert_eq!(
        pair.kept([2; 16]).stage,
        RemoteStage::Ended,
        "the grant ran out"
    );
    pair.pass();
    assert_eq!(pair.devices[&laptop].seen().placed, 0);

    // Another use; this time the phone leaves.
    pair.device(&phone).online = true;
    pair.ask([3; 16], &laptop, &phone, &microphone);
    pair.pass();
    pair.answer(&phone, true);
    pair.pass();
    assert_eq!(pair.devices[&laptop].seen().placed, 1);
    pair.device(&phone).online = false;
    pair.pass();
    assert_eq!(
        pair.kept([3; 16]).stage,
        RemoteStage::GivenUp,
        "the phone left"
    );
    pair.pass();
    assert_eq!(
        pair.devices[&laptop].seen().placed,
        0,
        "the virtual microphone is gone"
    );
}

/// A scanned document reaches the inbox whole, page by page; one past its pages or bytes is not
/// delivered, and the use goes on.
#[test]
fn a_remote_scanner_delivers_whole_documents_within_their_pages_and_bytes() {
    let (laptop, phone) = (laptop(), phone());
    let mut pair = Pair::new();
    let scanner = Profile::Scanner {
        format: DocumentFormat::Png,
        max_pages: 2,
        max_document_bytes: 200_000,
    };
    pair.ask([4; 16], &laptop, &phone, &scanner);
    pair.pass();
    pair.answer(&phone, true);
    pair.pass();
    let page = |fill: u8, len: usize| vec![fill; len];
    pair.device(&phone).seen().captured.extend([
        Unit::Page {
            bytes: page(1, 70_000),
            last: false,
        },
        Unit::Page {
            bytes: page(2, 10),
            last: true,
        },
    ]);
    pair.pass();
    pair.pass();
    assert_eq!(
        pair.devices[&laptop].seen().documents,
        [vec![page(1, 70_000), page(2, 10)]],
        "two pages, the first in two pieces"
    );

    // Three pages where two fit, then a document past its bytes: neither is delivered.
    pair.device(&phone).seen().captured.extend([
        Unit::Page {
            bytes: page(3, 10),
            last: false,
        },
        Unit::Page {
            bytes: page(4, 10),
            last: false,
        },
        Unit::Page {
            bytes: page(5, 10),
            last: true,
        },
    ]);
    pair.pass();
    pair.pass();
    pair.device(&phone).seen().captured.extend([
        Unit::Page {
            bytes: page(6, 150_000),
            last: false,
        },
        Unit::Page {
            bytes: page(7, 150_000),
            last: true,
        },
    ]);
    pair.pass();
    pair.pass();
    pair.device(&phone).seen().captured.push_back(Unit::Page {
        bytes: page(8, 5),
        last: true,
    });
    pair.pass();
    pair.pass();
    assert_eq!(
        pair.devices[&laptop].seen().documents,
        [vec![page(1, 70_000), page(2, 10)], vec![page(8, 5)]],
        "only whole documents within bounds arrive"
    );
    assert_eq!(
        pair.kept([4; 16]).stage,
        RemoteStage::Granted,
        "and the use goes on"
    );
}

/// With realtime sessions off, the phone declines without asking its person; turned off while a
/// use runs, it ends it. What a third device sends, or a message replayed, reaches nothing.
#[test]
fn realtime_sessions_off_declines_and_ends_and_only_the_granted_device_is_heard() {
    let (laptop, phone) = (laptop(), phone());
    let mut pair = Pair::new();
    pair.device(&phone).realtime = false;
    pair.ask([5; 16], &laptop, &phone, &camera());
    pair.pass();
    assert!(pair.devices[&phone].asked.is_empty(), "nobody is asked");
    assert_eq!(pair.kept([5; 16]).stage, RemoteStage::Declined);

    pair.device(&phone).realtime = true;
    pair.ask([6; 16], &laptop, &phone, &camera());
    pair.pass();
    pair.answer(&phone, true);
    pair.pass();
    pair.device(&phone)
        .seen()
        .captured
        .push_back(Unit::Frame(frame(9)));
    pair.pass();
    let sent = pair.devices[&laptop].heard.clone();
    let stranger = "44".repeat(32);
    pair.device(&laptop).heard = sent
        .iter()
        .map(|(_, body)| (stranger.clone(), body.clone()))
        .chain(sent.iter().cloned())
        .chain(sent.iter().cloned())
        .collect();
    pair.pass();
    assert_eq!(
        pair.devices[&laptop].seen().frames,
        [frame(9)],
        "the frame arrives once, from the phone"
    );

    pair.device(&phone).realtime = false;
    pair.pass();
    assert_eq!(
        pair.kept([6; 16]).stage,
        RemoteStage::Ended,
        "turned off, it ends"
    );
    pair.pass();
    assert_eq!(pair.devices[&phone].seen().capturing, 0);
    assert_eq!(pair.devices[&laptop].seen().placed, 0);
}

fn press(n: usize) -> Vec<ForwardedGamepadEvent> {
    (0..n)
        .map(|i| ForwardedGamepadEvent::Button {
            button: GamepadButton::South,
            pressed: i % 2 == 0,
        })
        .collect()
}

/// The laptop sends its controller to the phone. The phone's person decides with the game in
/// front, and their yes binds the controller to that very app session: a uinput pad drives it
/// there at no more than 240 events a second. The game leaving the front ends the use and the
/// pad goes; a lease lasts five minutes and is never renewed.
#[test]
fn a_controller_reaches_the_exact_app_in_front_at_its_rate_for_its_lease() {
    let (laptop, phone) = (laptop(), phone());
    let mut pair = Pair::new();
    pair.device(&phone).front = Some(game());
    pair.ask([7; 16], &laptop, &phone, &Profile::Controller);
    pair.pass();
    assert_eq!(
        pair.devices[&phone].asked[0].front,
        Some(game()),
        "asked with the game in front"
    );
    pair.answer(&phone, true);
    let bound = pair.kept([7; 16]);
    assert_eq!(
        (bound.app.as_str(), bound.session.as_str()),
        ("so.tilo.game", "window-7"),
        "the yes binds the exact app session"
    );
    pair.pass();
    assert_eq!(
        pair.devices[&phone].seen().pads,
        1,
        "a uinput pad takes its place"
    );

    let burst = usize::from(MAX_EVENTS_PER_SECOND) + 60;
    pair.device(&laptop).seen().pressed.extend(press(burst));
    pair.pass();
    pair.pass();
    let emitted = pair.devices[&phone].seen().emitted.len();
    assert_eq!(
        emitted,
        usize::from(MAX_EVENTS_PER_SECOND),
        "at most 240 a second"
    );

    // The lease runs five minutes from the yes, renewed by nothing.
    pair.at(GRANT_MS - 1);
    pair.pass();
    assert_eq!(pair.kept([7; 16]).stage, RemoteStage::Granted);
    pair.at(GRANT_MS + 1);
    pair.pass();
    pair.pass();
    assert!(pair.kept([7; 16]).stage.ended(), "the lease ran out");
    assert_eq!(pair.devices[&phone].seen().pads, 0, "the pad is gone");

    // Granted again, then the game leaves the front.
    pair.ask([8; 16], &laptop, &phone, &Profile::Controller);
    pair.pass();
    pair.answer(&phone, true);
    pair.pass();
    assert_eq!(pair.devices[&phone].seen().pads, 1);
    pair.device(&phone).front = Some(calls());
    pair.pass();
    assert_eq!(
        pair.kept([8; 16]).stage,
        RemoteStage::Ended,
        "the game lost focus"
    );
    assert_eq!(pair.devices[&phone].seen().pads, 0);
}

/// A controller's yes holds only for the app session it was asked with: another app in front by
/// the answer is a no. With nothing in front, or game controllers off, it is declined unasked.
#[test]
fn a_controller_is_declined_without_its_app_in_front_or_with_game_controllers_off() {
    let (laptop, phone) = (laptop(), phone());
    let mut pair = Pair::new();
    pair.device(&phone).front = Some(game());
    pair.ask([9; 16], &laptop, &phone, &Profile::Controller);
    pair.pass();
    pair.device(&phone).front = Some(calls());
    pair.answer(&phone, true);
    assert_eq!(
        pair.kept([9; 16]).stage,
        RemoteStage::Declined,
        "the game left before the yes"
    );

    pair.device(&phone).front = None;
    pair.at(1_000);
    pair.ask([10; 16], &laptop, &phone, &Profile::Controller);
    pair.pass();
    assert!(pair.devices[&phone].asked.is_empty());
    assert_eq!(
        pair.kept([10; 16]).stage,
        RemoteStage::Declined,
        "nothing in front"
    );

    pair.device(&phone).front = Some(game());
    pair.device(&phone).gamepads = false;
    pair.at(2_000);
    pair.ask([11; 16], &laptop, &phone, &Profile::Controller);
    pair.pass();
    assert!(pair.devices[&phone].asked.is_empty());
    assert_eq!(
        pair.kept([11; 16]).stage,
        RemoteStage::Declined,
        "game controllers off"
    );
    assert_eq!(pair.devices[&phone].seen().pads, 0);
}
