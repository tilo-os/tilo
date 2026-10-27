use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::*;
use crate::shell::device_fabric::FabricStamp;
use crate::shell::speaker_group::{SpeakerGroupMember, SpeakerRole, StereoSide};

fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}

fn microphone(device: &str) -> EndpointSelector {
    EndpointSelector::mint(&id(device), "mic")
}

/// A reading of `arrival_ms` at `level_db`, as a listening device sends it.
fn heard(arrival_ms: f64, level_db: f64) -> MicrophoneHearing {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "test arrivals are a few milliseconds"
    )]
    let nanoseconds = (arrival_ms * 1e6).round() as i64;
    MicrophoneHearing {
        hearing: SpeakerHearing::Heard {
            arrival_ns: nanoseconds,
            level_cdb: to_centi(level_db),
            snr_cdb: 3_000,
            response_cdb: Vec::new(),
        },
        clock_error_ns: 50_000,
    }
}

type Script = VecDeque<Vec<MicrophoneHearing>>;

/// A measurement source that answers from a script, one poll late, and
/// records every request it was given.
struct Fake {
    script: Script,
    pending: bool,
    requests: Arc<Mutex<Vec<SweepRequest>>>,
    cancelled: Arc<Mutex<bool>>,
}

impl MeasurementSource for Fake {
    fn begin(&mut self, request: &SweepRequest) -> Result<(), String> {
        self.requests.lock().unwrap().push(request.clone());
        self.pending = true;
        Ok(())
    }

    fn poll(&mut self) -> Option<Result<Vec<MicrophoneHearing>, String>> {
        if !self.pending {
            return None;
        }
        self.pending = false;
        self.script.pop_front().map(Ok)
    }

    fn cancel(&mut self) {
        self.pending = false;
        *self.cancelled.lock().unwrap() = true;
    }
}

fn pair() -> SpeakerGroup {
    let member = |device: &str, side| SpeakerGroupMember {
        device: id(device),
        output: EndpointSelector::mint(&id(device), "out"),
        role: SpeakerRole::Stereo(side),
        trim: SpeakerTrim {
            delay_ns: 1_000,
            gain_mb: -50,
            eq: Vec::new(),
        },
    };
    SpeakerGroup {
        id: SpeakerGroupId([3; 16]),
        name: "Living room".into(),
        leader: id("desk"),
        members: vec![
            member("desk", StereoSide::Left),
            member("shelf", StereoSide::Right),
        ],
        stamp: FabricStamp {
            counter: 1,
            device: id("desk"),
        },
    }
}

fn run_microphone(device: &str, consent: MicrophoneConsent) -> RunMicrophone {
    RunMicrophone {
        device: id(device),
        microphone: microphone(device),
        label: format!("{device} microphone"),
        consent,
    }
}

struct Harness {
    run: CalibrationRun,
    requests: Arc<Mutex<Vec<SweepRequest>>>,
    cancelled: Arc<Mutex<bool>>,
}

/// Desk and shelf measured from the phone on the sofa, with desk's and
/// shelf's own microphones listening too.
fn start(script: Script, consents: [MicrophoneConsent; 3], now: Instant) -> Harness {
    start_group(&pair(), script, consents, now)
}

fn start_group(
    group: &SpeakerGroup,
    script: Script,
    consents: [MicrophoneConsent; 3],
    now: Instant,
) -> Harness {
    let labels = group
        .members
        .iter()
        .map(|member| {
            let name = member.device.as_str();
            name[..1].to_uppercase() + &name[1..]
        })
        .collect::<Vec<_>>();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let cancelled = Arc::new(Mutex::new(false));
    let mut sweeps = 0_u8;
    let run = CalibrationRun::start(
        group,
        &labels,
        vec![
            run_microphone("desk", consents[0]),
            run_microphone("shelf", consents[1]),
            run_microphone("phone", consents[2]),
        ],
        2,
        Box::new(Fake {
            script,
            pending: false,
            requests: Arc::clone(&requests),
            cancelled: Arc::clone(&cancelled),
        }),
        Box::new(move || {
            sweeps = sweeps.saturating_add(1);
            [sweeps; 16]
        }),
        now,
    )
    .unwrap();
    Harness {
        run,
        requests,
        cancelled,
    }
}

const ALL_GIVEN: [MicrophoneConsent; 3] = [MicrophoneConsent::Given; 3];

/// Desk's output takes 20 ms, shelf's 45 ms; the sofa is 2.7 m from desk
/// and 2 m from shelf.
fn living_room() -> Script {
    VecDeque::from([
        // Desk plays: its own microphone, shelf's, the phone on the sofa.
        vec![heard(20.3, -1.0), heard(31.0, -12.0), heard(28.0, -6.0)],
        vec![heard(56.0, -13.0), heard(45.3, -2.0), heard(51.0, -9.0)],
    ])
}

#[test]
fn each_speaker_is_measured_from_the_listening_position() {
    let now = Instant::now();
    let mut harness = start(living_room(), ALL_GIVEN, now);
    let run = &mut harness.run;
    assert_eq!(run.phase(), CalibrationPhase::Measuring);
    assert_eq!(run.progress()[0], MemberProgress::Measuring);
    assert_eq!(run.progress()[1], MemberProgress::Waiting);
    {
        let asked = harness.requests.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].member, id("desk"));
        assert_eq!(
            asked[0].microphones,
            vec![
                (id("desk"), microphone("desk")),
                (id("shelf"), microphone("shelf")),
                (id("phone"), microphone("phone")),
            ],
            "every microphone hears the same sweep"
        );
    }
    assert!(run.tick());
    assert!(run.tick());
    assert_eq!(run.phase(), CalibrationPhase::Proposed);
    let asked = harness.requests.lock().unwrap().clone();
    assert_ne!(asked[0].sweep, asked[1].sweep, "every sweep has its own id");
    assert!(*harness.cancelled.lock().unwrap(), "clock sync is released");

    // Arrivals at the sofa decide: desk waits 23 ms for shelf and is cut
    // 3 dB to match it.
    let after = run.after().unwrap();
    assert_eq!(after[0].delay_ns, 23_000_000);
    assert_eq!(after[0].gain_mb, -300);
    assert_eq!(after[1].delay_ns, 0);
    assert_eq!(after[1].gain_mb, 0);
    let MemberProgress::Measured(desk) = &run.progress()[0] else {
        panic!("{:?}", run.progress()[0]);
    };
    assert_eq!(desk.arrival_ns, 28_000_000);
    assert_eq!(desk.heard_by, 3);
    assert_eq!(desk.output_latency_ns, Some(20_300_000));
    assert_eq!(desk.acoustic_ns, Some(7_700_000));
    assert_eq!(
        run.microphone_progress(),
        &[
            MicrophoneProgress::Used {
                heard: 2,
                rejected: 0
            },
            MicrophoneProgress::Used {
                heard: 2,
                rejected: 0
            },
            MicrophoneProgress::Used {
                heard: 2,
                rejected: 0
            },
        ]
    );
    assert_eq!(
        run.before()[0].delay_ns,
        1_000,
        "before keeps current trims"
    );
}

#[test]
fn a_listening_reading_the_other_microphones_contradict_is_set_aside() {
    let now = Instant::now();
    // A subwoofer joins: 30 ms of output, 1.5 m from the sofa.
    let mut trio = pair();
    let mut sub = trio.members[1].clone();
    sub.device = id("sub");
    sub.output = EndpointSelector::mint(&id("sub"), "out");
    trio.members.push(sub);
    let mut script = living_room();
    script.push_back(vec![
        heard(38.7, -8.0),
        heard(37.3, -7.0),
        heard(34.4, -4.0),
    ]);
    // A missed direct sound at the sofa: a reflection 40 ms late.
    script[1][2] = heard(91.0, -9.0);
    let mut harness = start_group(&trio, script, ALL_GIVEN, now);
    for _ in 0..3 {
        harness.run.tick();
    }
    assert_eq!(harness.run.phase(), CalibrationPhase::Proposed);
    let MemberProgress::Measured(shelf) = &harness.run.progress()[1] else {
        panic!("{:?}", harness.run.progress()[1]);
    };
    assert!(
        (shelf.arrival_ns - 51_000_000).abs() < 15_000_000,
        "estimated from the other microphones, within the room's path differences: {}",
        shelf.arrival_ns
    );
    assert_eq!(
        harness.run.microphone_progress()[2],
        MicrophoneProgress::Used {
            heard: 3,
            rejected: 1
        }
    );
    let MemberProgress::Measured(desk) = &harness.run.progress()[0] else {
        panic!("{:?}", harness.run.progress()[0]);
    };
    assert_eq!(desk.arrival_ns, 28_000_000, "the other seat readings stand");
}

#[test]
fn a_pair_s_seat_readings_stand_even_when_the_others_disagree() {
    let now = Instant::now();
    let mut script = living_room();
    script[1][2] = heard(91.0, -9.0);
    let mut harness = start(script, ALL_GIVEN, now);
    harness.run.tick();
    harness.run.tick();
    let MemberProgress::Measured(shelf) = &harness.run.progress()[1] else {
        panic!("{:?}", harness.run.progress()[1]);
    };
    assert_eq!(shelf.arrival_ns, 91_000_000);
}

#[test]
fn nothing_is_applied_until_asked_and_revert_restores_the_old_trims() {
    let now = Instant::now();
    let mut harness = start(living_room(), ALL_GIVEN, now);
    let run = &mut harness.run;
    run.tick();
    run.tick();
    assert!(run.previous_trims().is_none(), "nothing applied yet");
    let proposed = run.proposed_trims().unwrap();
    assert_eq!(proposed[0].2.delay_ns, 23_000_000);
    run.mark_applied();
    assert!(run.proposed_trims().is_none());
    let previous = run.previous_trims().unwrap();
    assert_eq!(previous[0].2.delay_ns, 1_000);
    assert_eq!(previous[1].0, id("shelf"));
}

#[test]
fn a_member_no_microphone_heard_fails_the_run_with_its_name() {
    let now = Instant::now();
    let not_heard = || MicrophoneHearing::failed(HearingFailure::NoArrival);
    let mut harness = start(
        VecDeque::from([
            vec![heard(20.3, -1.0), heard(31.0, -12.0), heard(28.0, -6.0)],
            vec![not_heard(), not_heard(), not_heard()],
        ]),
        ALL_GIVEN,
        now,
    );
    harness.run.tick();
    harness.run.tick();
    assert_eq!(harness.run.phase(), CalibrationPhase::Failed);
    assert_eq!(
        harness.run.message(),
        Some("Could not measure Shelf: no sweep stood out of the noise there")
    );
    assert!(harness.run.proposed_trims().is_none());
}

#[test]
fn measuring_waits_for_microphone_consent_and_leaves_out_who_did_not_answer() {
    let now = Instant::now();
    let mut harness = start(
        living_room(),
        [
            MicrophoneConsent::Given,
            MicrophoneConsent::Asked,
            MicrophoneConsent::Asked,
        ],
        now,
    );
    assert_eq!(harness.run.phase(), CalibrationPhase::WaitingForMicrophones);
    assert!(
        harness.requests.lock().unwrap().is_empty(),
        "nothing plays yet"
    );
    assert!(!harness.run.tick());
    // The phone agrees; shelf never answers.
    let answers = |device: &DeviceId| {
        if device == &id("phone") {
            MicrophoneConsent::Given
        } else {
            MicrophoneConsent::Asked
        }
    };
    assert!(harness.run.update_consent(answers, now));
    assert_eq!(harness.run.phase(), CalibrationPhase::WaitingForMicrophones);
    assert_eq!(
        harness.run.microphone_progress()[2],
        MicrophoneProgress::Listening
    );
    assert!(
        harness
            .run
            .update_consent(answers, now + MICROPHONE_CONSENT_WAIT)
    );
    assert_eq!(harness.run.phase(), CalibrationPhase::Measuring);
    assert_eq!(
        harness.run.microphone_progress()[1],
        MicrophoneProgress::Declined
    );
    let asked = harness.requests.lock().unwrap().clone();
    assert_eq!(
        asked[0].microphones,
        vec![
            (id("desk"), microphone("desk")),
            (id("phone"), microphone("phone")),
        ]
    );
}

#[test]
fn a_refused_listening_microphone_ends_the_run() {
    let now = Instant::now();
    let mut harness = start(
        living_room(),
        [
            MicrophoneConsent::Given,
            MicrophoneConsent::Given,
            MicrophoneConsent::Asked,
        ],
        now,
    );
    assert!(
        harness
            .run
            .update_consent(|_| MicrophoneConsent::Refused, now)
    );
    assert_eq!(harness.run.phase(), CalibrationPhase::Failed);
    assert_eq!(
        harness.run.message(),
        Some(
            "phone microphone was not allowed to listen, so nothing can be measured from where you sit"
        )
    );
    assert!(harness.requests.lock().unwrap().is_empty());
}

#[test]
fn a_refused_listening_microphone_cannot_start_a_run() {
    let refused = CalibrationRun::start(
        &pair(),
        &[],
        vec![run_microphone("phone", MicrophoneConsent::Refused)],
        0,
        Box::new(Fake {
            script: VecDeque::new(),
            pending: false,
            requests: Arc::default(),
            cancelled: Arc::default(),
        }),
        Box::new(|| [1; 16]),
        Instant::now(),
    );
    assert!(refused.is_err());
}

#[test]
fn cancelling_stops_measuring_and_a_changed_group_no_longer_matches() {
    let mut harness = start(VecDeque::new(), ALL_GIVEN, Instant::now());
    assert!(harness.run.matches(&pair()));
    harness.run.cancel();
    assert_eq!(harness.run.phase(), CalibrationPhase::Failed);
    assert!(!harness.run.tick());
    let mut changed = pair();
    changed.members.pop();
    assert!(!harness.run.matches(&changed));
}

#[test]
fn a_hearing_crosses_the_fabric_as_bounded_numbers() {
    let sweep = sweep().unwrap();
    let mut capture = vec![0.0_f32; 9_600];
    capture.extend_from_slice(sweep.signal());
    capture.extend(std::iter::repeat_n(0.0, 48_000));
    let measurement = sweep.measure(&capture, 0, 200_000_000).unwrap();
    let hearing = SpeakerHearing::from_measurement(&measurement);
    assert!(hearing.valid());
    let back = hearing.measurement().unwrap();
    assert_eq!(back.arrival_ns, measurement.arrival_ns);
    assert!((back.level_db - measurement.level_db).abs() < 0.01);
    assert!((back.snr_db - measurement.snr_db).abs() < 0.01);
    assert_eq!(
        back.response.points.len(),
        measurement.response.points.len()
    );
    assert!(
        back.response
            .points
            .iter()
            .zip(&measurement.response.points)
            .all(|(a, b)| (a.hz - b.hz).abs() < 1e-6 && (a.db - b.db).abs() < 0.01)
    );
    let oversized = SpeakerHearing::Heard {
        arrival_ns: 0,
        level_cdb: 0,
        snr_cdb: 0,
        response_cdb: vec![0; MAX_RESPONSE_POINTS + 1],
    };
    assert!(!oversized.valid());
    let absurd = SpeakerHearing::Heard {
        arrival_ns: 5_000_000_000,
        level_cdb: 0,
        snr_cdb: 0,
        response_cdb: Vec::new(),
    };
    assert!(!absurd.valid());
}

/// A listening device whose clock runs 150 ppm fast and 3.2 s ahead is
/// scheduled through the fabric clock estimate; its recording, measured
/// against that schedule, reads the true acoustic delay.
#[test]
fn a_skewed_listening_clock_is_scheduled_through_the_fabric_estimate() {
    const SKEW: f64 = 150e-6;
    const OFFSET_NS: f64 = 3_200_000_000.0;
    let peer_ns = |local_ns: f64| local_ns.mul_add(1.0 + SKEW, OFFSET_NS);
    let as_us = |ns: f64| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test clocks are positive and far inside u64"
        )]
        let us = (ns / 1_000.0).round() as u64;
        us
    };
    // Twelve seconds of probes, 250 ms apart, 400 us each way.
    let mut estimator = crate::shell::fabric_clock::PeerClockEstimator::default();
    let start_ns = 50_000_000_000.0;
    for step in 0..48_u32 {
        let t1 = start_ns + f64::from(step) * 250_000_000.0;
        let t2 = peer_ns(t1 + 400_000.0);
        assert!(estimator.observe(crate::shell::fabric_clock::ClockSample {
            t1: as_us(t1),
            t2: as_us(t2),
            t3: as_us(t2 + 50_000.0),
            t4: as_us(t1 + 850_000.0),
        }));
    }
    // The sweep plays twenty seconds after the last probe.
    let play_at_ns = 82_000_000_123_i64;
    #[expect(
        clippy::cast_precision_loss,
        reason = "test times are exact in f64 to well under a microsecond"
    )]
    let play_at = play_at_ns as f64;
    let clock = estimator.estimate(as_us(play_at)).unwrap();
    assert!(clock.synced);
    let (scheduled_ns, error_ns) = schedule_on_peer(&clock, play_at_ns).unwrap();
    assert!(error_ns > 0, "the bound travels with the reading");

    // The sweep leaves the speaker at the true instant and reaches the
    // microphone 350 samples (7.29 ms) later; the recording starts 9 600
    // samples (exactly 200 ms) before the true instant.
    let emitted_on_peer = peer_ns(play_at);
    let sweep = sweep().unwrap();
    let mut samples = vec![0_i16; 9_600 + 350];
    samples.extend(sweep.signal().iter().map(|sample| {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the sweep is bounded to half of full scale"
        )]
        let value = (sample * 32_767.0) as i16;
        value
    }));
    samples.extend(std::iter::repeat_n(0, 40_000));
    #[expect(
        clippy::cast_possible_truncation,
        reason = "test times are far inside i64"
    )]
    let first_sample_ns = (emitted_on_peer - 200_000_000.0).round() as i64;
    let hearing = hear(&samples, first_sample_ns, scheduled_ns);
    let SpeakerHearing::Heard { arrival_ns, .. } = hearing else {
        panic!("{hearing:?}");
    };
    assert!(
        (arrival_ns - 7_291_667).abs() < 50_000,
        "arrival {arrival_ns} ns"
    );

    // Ignoring the skew would misplace the recording by milliseconds.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "test times are far inside i64"
    )]
    let offset_only = (play_at + OFFSET_NS + SKEW * start_ns).round() as i64;
    assert!((offset_only - scheduled_ns).abs() > 1_000_000);
}
