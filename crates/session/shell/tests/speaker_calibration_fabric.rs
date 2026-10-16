//! Speaker-group calibration across fake devices of one person.
//!
//! A desk leads a stereo pair with a shelf speaker; the person sits on the
//! sofa with a phone. Each device is its own speaker-group policy core
//! publishing into a shared fabric, and the room is simulated: every
//! microphone that its own device let listen hears each sweep after that
//! member's output latency plus the flight time from the speaker. The run
//! must measure from the sofa, use the desk's own microphone to split the
//! desk's delay into output latency and distance, leave out the shelf's
//! microphone because the shelf declined, and never list a device outside
//! the person's fabric.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use shell::shell::device_fabric::{
    DeviceFabric, DeviceFabricSnapshot, DeviceKind, DevicePresence, FABRIC_SCHEMA_VERSION,
    FabricCapabilities, FabricStamp, PersonalFabricAction,
};
use shell::shell::personal_continuity::DeviceId;
use shell::shell::speaker_calibration::{
    CalibrationPhase, CalibrationRun, HearingFailure, MeasurementSource, MemberProgress,
    MicrophoneConsent, MicrophoneHearing, MicrophoneProgress, SpeakerHearing, SweepRequest,
};
use shell::shell::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerGroupId, SpeakerRole, StereoSide,
};
use shell::shell::speaker_group_runtime::{Egress, ListenerState, MemberSpec, SpeakerGroupCore};
use shell::shell::speaker_settings::plan_microphones;

const SPEED_OF_SOUND_M_PER_S: f64 = 343.0;

fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}

fn output(device: &str) -> EndpointSelector {
    EndpointSelector::mint(&id(device), "alsa_output.analog-stereo")
}

fn microphone(device: &str) -> EndpointSelector {
    EndpointSelector::mint(&id(device), "alsa_input.analog-stereo")
}

fn stamp(device: &str) -> FabricStamp {
    FabricStamp {
        counter: 0,
        device: id(device),
    }
}

/// One fake device: its policy core, where it stands, and how long its
/// output takes.
struct Device {
    name: &'static str,
    core: SpeakerGroupCore,
    counter: u64,
    position_m: (f64, f64),
    output_latency_ms: f64,
}

impl Device {
    fn new(name: &'static str, position_m: (f64, f64), output_latency_ms: f64) -> Self {
        let mut core = SpeakerGroupCore::default();
        core.seed(None);
        Self {
            name,
            core,
            counter: 0,
            position_m,
            output_latency_ms,
        }
    }

    fn id(&self) -> DeviceId {
        id(self.name)
    }

    fn publish(&mut self, fabric: &mut DeviceFabric) {
        self.counter = self.counter.saturating_add(1);
        let owner = self.id();
        let endpoints = vec![
            AudioEndpoint {
                selector: output(self.name),
                kind: AudioEndpointKind::Output,
                label: "Speakers".into(),
                is_default: true,
            },
            AudioEndpoint {
                selector: microphone(self.name),
                kind: AudioEndpointKind::Microphone,
                label: "Microphone".into(),
                is_default: true,
            },
        ];
        let snapshot = DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: owner.clone(),
            presence: DevicePresence {
                device: owner.clone(),
                label: self.name.into(),
                kind: DeviceKind::Speaker,
                capabilities: FabricCapabilities {
                    audio_output: true,
                    audio_group: true,
                    ..FabricCapabilities::default()
                },
                online: true,
                last_seen_unix_ms: self.counter,
                wake_routes: Vec::new(),
                stamp: FabricStamp {
                    counter: self.counter,
                    device: owner,
                },
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
            unlock: shell::shell::device_unlock::UnlockFabricState::default(),
            speakers: self.core.authored(endpoints, &stamp(self.name)),
        };
        assert_eq!(fabric.upsert(snapshot), Ok(true));
    }
}

/// The person's devices, by name, and the fabric they publish into.
struct Home {
    devices: BTreeMap<&'static str, Device>,
    fabric: DeviceFabric,
}

impl Home {
    fn device(&mut self, name: &str) -> &mut Device {
        self.devices.get_mut(name).unwrap()
    }

    fn publish(&mut self, name: &str) {
        let device = self.devices.get_mut(name).unwrap();
        device.publish(&mut self.fabric);
    }

    /// Deliver every egress from `from` and return what the targets sent
    /// back.
    fn deliver(&mut self, from: &str, out: Vec<Egress>, now: Instant) -> Vec<(String, Egress)> {
        let mut replies = Vec::new();
        for item in out {
            let target = item.target.as_str().to_string();
            let device = self.devices.get_mut(target.as_str()).unwrap();
            let mut mint = || [7; 16];
            let answer = device
                .core
                .handle(
                    &self.fabric,
                    &device.id(),
                    &id(from),
                    &item.action,
                    now,
                    &mut mint,
                )
                .expect("a speaker-group action");
            replies.extend(answer.into_iter().map(|reply| (target.clone(), reply)));
        }
        replies
    }
}

/// Where every device's policy core stands, shared with the simulated
/// room so it asks each listening device for admission exactly as a
/// `SpeakerListen` would.
type Admission = Arc<Mutex<Box<dyn Fn(&DeviceId, EndpointSelector) -> bool + Send>>>;

/// The room: every sweep is heard by every admitted microphone.
struct Room {
    speakers: BTreeMap<String, ((f64, f64), f64)>,
    microphones: BTreeMap<String, (f64, f64)>,
    admitted: Admission,
    heard: Option<Vec<MicrophoneHearing>>,
    asked: Arc<Mutex<Vec<SweepRequest>>>,
}

fn distance((ax, ay): (f64, f64), (bx, by): (f64, f64)) -> f64 {
    (ax - bx).hypot(ay - by)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "simulated arrivals and levels are small"
)]
fn round(value: f64) -> i64 {
    value.round() as i64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "simulated times are far inside f64 precision"
)]
fn ms(ns: i64) -> f64 {
    ns as f64 / 1e6
}

impl MeasurementSource for Room {
    fn begin(&mut self, request: &SweepRequest) -> Result<(), String> {
        self.asked
            .lock()
            .map_err(|error| error.to_string())?
            .push(request.clone());
        let (position, latency_ms) = self.speakers[request.member.as_str()];
        let admitted = self.admitted.lock().map_err(|error| error.to_string())?;
        self.heard = Some(
            request
                .microphones
                .iter()
                .map(|(device, selector)| {
                    if !admitted(device, *selector) {
                        return MicrophoneHearing::failed(HearingFailure::NotAllowed);
                    }
                    let metres = distance(position, self.microphones[device.as_str()]);
                    let flight_ms = metres / SPEED_OF_SOUND_M_PER_S * 1e3;
                    let hearing = SpeakerHearing::Heard {
                        arrival_ns: round((latency_ms + flight_ms) * 1e6),
                        level_cdb: i32::try_from(round(-600.0 - 2_000.0 * metres.max(0.1).log10()))
                            .unwrap(),
                        snr_cdb: 3_500,
                        response_cdb: Vec::new(),
                    };
                    MicrophoneHearing {
                        hearing,
                        clock_error_ns: 80_000,
                    }
                })
                .collect(),
        );
        // Every simulated reading must be one the fabric admits.
        let admissible = self
            .heard
            .iter()
            .flatten()
            .all(|reading| reading.hearing.valid());
        admissible
            .then_some(())
            .ok_or_else(|| "a reading the fabric would refuse".to_string())
    }

    fn poll(&mut self) -> Option<Result<Vec<MicrophoneHearing>, String>> {
        self.heard.take().map(Ok)
    }

    fn cancel(&mut self) {
        self.heard = None;
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one calibration followed across every device, from pairing to applied trims"
)]
fn calibration_measures_each_speaker_from_the_sofa_with_consenting_microphones() {
    let now = Instant::now();
    let mut home = Home {
        devices: BTreeMap::from([
            ("desk", Device::new("desk", (-1.5, 3.0), 20.0)),
            ("shelf", Device::new("shelf", (1.5, 2.0), 45.0)),
            ("phone", Device::new("phone", (0.0, 0.0), 0.0)),
        ]),
        fabric: DeviceFabric::default(),
    };
    for name in ["desk", "shelf", "phone"] {
        home.publish(name);
    }
    let group = SpeakerGroupId([0x42; 16]);
    let desk_id = id("desk");

    // The pair forms: shelf agrees to play.
    let invites = home
        .devices
        .get_mut("desk")
        .unwrap()
        .core
        .create_group(
            &home.fabric,
            &desk_id,
            group,
            "Living room",
            &[
                MemberSpec {
                    device: id("desk"),
                    output: output("desk"),
                    role: SpeakerRole::Stereo(StereoSide::Left),
                },
                MemberSpec {
                    device: id("shelf"),
                    output: output("shelf"),
                    role: SpeakerRole::Stereo(StereoSide::Right),
                },
            ],
            now,
        )
        .unwrap();
    home.publish("desk");
    home.deliver("desk", invites, now);
    let joined = {
        let fabric = &home.fabric;
        let shelf = home.devices.get_mut("shelf").unwrap();
        shelf
            .core
            .resolve_consent(fabric, &id("shelf"), group, true, &stamp("shelf"))
            .unwrap()
    };
    home.publish("shelf");
    home.deliver("shelf", joined, now);

    // Settings: measure from the phone on the sofa, with every microphone.
    let published = home.fabric.speaker_group(group).unwrap().clone();
    let candidates =
        home.devices["desk"]
            .core
            .listener_candidates(&home.fabric, &desk_id, &published, now);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| (candidate.device.as_str().to_string(), candidate.state))
            .collect::<Vec<_>>(),
        vec![
            ("desk".into(), ListenerState::Ready),
            ("phone".into(), ListenerState::NotAsked),
            ("shelf".into(), ListenerState::NotAsked),
        ],
        "only the person's devices, this one first; playing is not listening"
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.device != id("guest"))
    );
    let (microphones, listening) = plan_microphones(&candidates, &[], Some(&id("phone"))).unwrap();
    assert_eq!(microphones[listening].device, id("phone"));
    let ask = microphones
        .iter()
        .filter(|microphone| microphone.consent == MicrophoneConsent::Asked)
        .map(|microphone| (microphone.device.clone(), microphone.microphone))
        .collect::<Vec<_>>();
    let listen_invites = home
        .device("desk")
        .core
        .invite_listeners(&published, &ask, now);
    assert_eq!(listen_invites.len(), 2);
    assert!(listen_invites.iter().all(|invite| matches!(
        invite.action,
        PersonalFabricAction::SpeakerListenInvite { .. }
    )));

    // The room asks each listening device's own core, as SpeakerListen does.
    let admitted: Admission = Arc::new(Mutex::new(Box::new(|_, _| false)));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut sweeps = 0_u8;
    let mut run = CalibrationRun::start(
        &published,
        &["Desk".into(), "Shelf".into()],
        microphones,
        listening,
        Box::new(Room {
            speakers: home
                .devices
                .values()
                .map(|device| {
                    (
                        device.name.to_string(),
                        (device.position_m, device.output_latency_ms),
                    )
                })
                .collect(),
            microphones: home
                .devices
                .values()
                .map(|device| (device.name.to_string(), device.position_m))
                .collect(),
            admitted: Arc::clone(&admitted),
            heard: None,
            asked: Arc::clone(&asked),
        }),
        Box::new(move || {
            sweeps += 1;
            [sweeps; 16]
        }),
        now,
    )
    .unwrap();
    assert_eq!(run.phase(), CalibrationPhase::WaitingForMicrophones);

    // The phone allows its microphone; the shelf declines.
    home.deliver("desk", listen_invites, now);
    let mut answers = Vec::new();
    for (name, approved) in [("phone", true), ("shelf", false)] {
        let fabric = &home.fabric;
        let device = home.devices.get_mut(name).unwrap();
        let view = device.core.view(fabric, &device.id(), &|_| None);
        assert!(
            view.consents.iter().any(|consent| consent.group == group
                && consent.prompt() == "Let desk use this microphone for Living room?"),
            "{name} asks its own user: {:?}",
            view.consents
        );
        answers.extend(
            device
                .core
                .resolve_listen_consent(fabric, &device.id(), group, approved, &stamp(name))
                .unwrap()
                .into_iter()
                .map(|reply| (name, reply)),
        );
    }
    home.publish("phone");
    home.publish("shelf");
    for (name, reply) in answers {
        home.deliver(name, vec![reply], now);
    }

    let states = home.devices["desk"]
        .core
        .listener_candidates(&home.fabric, &desk_id, &published, now)
        .into_iter()
        .map(|candidate| (candidate.device, candidate.state))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(states[&id("phone")], ListenerState::Ready);
    assert_eq!(states[&id("shelf")], ListenerState::Declined);

    // Admission as each listening device decides it for a SpeakerListen.
    {
        let fabric = home.fabric.clone();
        let cores = ["phone", "shelf"]
            .into_iter()
            .map(|name| {
                let device = home.devices.remove(name).unwrap();
                (device.id(), device)
            })
            .collect::<BTreeMap<_, _>>();
        let desk = desk_id;
        *admitted.lock().unwrap() = Box::new(move |device, selector| {
            device == &desk
                || cores.get(device).is_some_and(|listener| {
                    listener
                        .core
                        .may_listen(&fabric, device, &desk, group, selector)
                })
        });
    }

    let consent = |device: &DeviceId| match states.get(device) {
        Some(ListenerState::Ready) => MicrophoneConsent::Given,
        Some(ListenerState::NotAsked | ListenerState::AwaitingConsent) => MicrophoneConsent::Asked,
        _ => MicrophoneConsent::Refused,
    };
    assert!(run.update_consent(consent, now));
    assert_eq!(run.phase(), CalibrationPhase::Measuring);
    let mut ticks = 0;
    while run.phase() == CalibrationPhase::Measuring && ticks < 10 {
        run.tick();
        ticks += 1;
    }
    assert_eq!(
        run.phase(),
        CalibrationPhase::Proposed,
        "{:?}",
        run.message()
    );

    // Only consenting microphones were ever asked to listen.
    for request in asked.lock().unwrap().iter() {
        assert_eq!(
            request.microphones,
            vec![
                (id("desk"), microphone("desk")),
                (id("phone"), microphone("phone"))
            ]
        );
    }
    assert_eq!(run.microphone_progress()[2], MicrophoneProgress::Declined);

    // Desk: 20 ms of output + 3.35 m of air; shelf: 45 ms + 2.5 m.
    let flight_ms = |position| distance(position, (0.0, 0.0)) / SPEED_OF_SOUND_M_PER_S * 1e3;
    let desk_at_sofa = 20.0 + flight_ms((-1.5, 3.0));
    let shelf_at_sofa = 45.0 + flight_ms((1.5, 2.0));
    let MemberProgress::Measured(desk) = &run.progress()[0] else {
        panic!("{:?}", run.progress()[0]);
    };
    assert!((ms(desk.arrival_ns) - desk_at_sofa).abs() < 0.01);
    assert_eq!(desk.output_latency_ns, Some(20_000_000));
    let acoustic_m = ms(desk.acoustic_ns.unwrap()) / 1e3 * SPEED_OF_SOUND_M_PER_S;
    assert!((acoustic_m - 3.354).abs() < 0.01, "{acoustic_m} m");
    let MemberProgress::Measured(shelf) = &run.progress()[1] else {
        panic!("{:?}", run.progress()[1]);
    };
    assert_eq!(shelf.output_latency_ns, None, "shelf kept its microphone");

    // Applying stores trims that make both arrive together at the sofa.
    let trims = run.proposed_trims().unwrap();
    let desk_delay_ms = ms(i64::try_from(trims[0].2.delay_ns).unwrap());
    assert!((desk_at_sofa + desk_delay_ms - shelf_at_sofa).abs() < 0.01);
    assert_eq!(trims[1].2.delay_ns, 0);
    home.device("desk").core.set_trims(group, &trims).unwrap();
    home.publish("desk");
    let stored = home.fabric.speaker_group(group).unwrap();
    assert_eq!(stored.members[0].trim.delay_ns, trims[0].2.delay_ns);
}
