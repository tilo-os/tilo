//! Speaker groups keep time on the fabric clock, and the fabric clock is part
//! of Media continuity. With Media on and Display & input off, a group syncs
//! its clock and plays; with Media off it refuses with a reason; and a member
//! whose clock never arrives is reported, never left playing silence.

use std::time::{Duration, Instant};

use serde_json::json;
use shell::shell::continuity_policy::ContinuityPolicySnapshot;
use shell::shell::device_fabric::{
    DeviceFabric, DeviceFabricSnapshot, DeviceKind, DevicePresence, FABRIC_SCHEMA_VERSION,
    FabricCapabilities, FabricStamp, OwnerSession, PersonalFabricAction,
};
use shell::shell::fabric_clock::{
    CLOCK_SYNC_DEADLINE, ClockSyncFailure, ClockSyncState, FabricClockCore,
};
use shell::shell::personal_continuity::DeviceId;
use shell::shell::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerGroupId, SpeakerRole, StereoSide,
};
use shell::shell::speaker_group_runtime::{
    Egress, GroupAudioSource, MemberSpec, SpeakerGroupCore, SpeakerSessionStatus, member_route_id,
};

const SESSION: [u8; 16] = [8; 16];

fn id(name: &str) -> DeviceId {
    DeviceId::new(name).unwrap()
}

fn output(name: &str) -> EndpointSelector {
    EndpointSelector::mint(&id(name), "alsa_output.analog-stereo")
}

fn stamp(name: &str, counter: u64) -> FabricStamp {
    FabricStamp {
        counter,
        device: id(name),
    }
}

fn policy(media: bool, display_input: bool) -> ContinuityPolicySnapshot {
    let document = json!({
        "continuity": {"enabled": true, "media": media, "display_input": display_input}
    });
    ContinuityPolicySnapshot::from_document(document.as_object().unwrap())
}

/// One simulated device: its speaker-group core, its fabric clock and the
/// continuity policy its fabric runtime admits actions under.
struct Device {
    name: &'static str,
    core: SpeakerGroupCore,
    clock: FabricClockCore,
    policy: ContinuityPolicySnapshot,
    counter: u64,
}

impl Device {
    fn new(name: &'static str, policy: ContinuityPolicySnapshot, now: Instant) -> Self {
        let mut core = SpeakerGroupCore::default();
        core.seed(None);
        let mut clock = FabricClockCore::default();
        clock.set_enabled(policy.permits_action(&probe()), now);
        Self {
            name,
            core,
            clock,
            policy,
            counter: 0,
        }
    }

    fn id(&self) -> DeviceId {
        id(self.name)
    }

    fn publish(&mut self, fabric: &mut DeviceFabric) {
        self.counter = self.counter.saturating_add(1);
        let owner = self.id();
        let endpoints = vec![AudioEndpoint {
            selector: output(self.name),
            kind: AudioEndpointKind::Output,
            label: format!("{} speakers", self.name),
            is_default: true,
        }];
        let snapshot = DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: owner.clone(),
            presence: DevicePresence {
                device: owner,
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
                session: OwnerSession::default(),
                stamp: stamp(self.name, self.counter),
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
            unlock: shell::shell::device_unlock::UnlockFabricState::default(),
            speakers: self
                .core
                .authored(endpoints, &stamp(self.name, self.counter)),
        };
        assert_eq!(fabric.upsert(snapshot), Ok(true));
    }

    /// Admit one action as the fabric runtime does, through this device's
    /// continuity policy, then hand it to the speaker-group core.
    fn deliver(
        &mut self,
        fabric: &DeviceFabric,
        item: &Egress,
        from: &str,
        now: Instant,
    ) -> Vec<Egress> {
        assert_eq!(item.target, self.id());
        assert!(
            self.policy.permits_action(&item.action),
            "{:?} must be admitted under Media",
            item.action
        );
        let mut mint = || [9; 16];
        self.core
            .handle(fabric, &self.id(), &id(from), &item.action, now, &mut mint)
            .expect("a speaker-group action")
    }
}

fn probe() -> PersonalFabricAction {
    PersonalFabricAction::FabricClockProbe {
        nonce: [1; 16],
        sent_us: 1,
    }
}

fn members() -> Vec<MemberSpec> {
    vec![
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
    ]
}

/// Desk leads a consented stereo pair with shelf and starts playing to it;
/// shelf admits the session.
fn playing_pair(
    desk: &mut Device,
    shelf: &mut Device,
    now: Instant,
) -> (DeviceFabric, SpeakerGroupId) {
    let mut fabric = DeviceFabric::default();
    desk.publish(&mut fabric);
    shelf.publish(&mut fabric);
    let group = SpeakerGroupId([4; 16]);
    let invites = desk
        .core
        .create_group(&fabric, &desk.id(), group, "Living room", &members(), now)
        .unwrap();
    desk.publish(&mut fabric);
    assert!(shelf.deliver(&fabric, &invites[0], "desk", now).is_empty());
    let decision = shelf
        .core
        .resolve_consent(&fabric, &shelf.id(), group, true, &stamp("shelf", 99))
        .unwrap();
    shelf.publish(&mut fabric);
    assert!(desk.deliver(&fabric, &decision[0], "shelf", now).is_empty());

    let offers = desk
        .core
        .start_session(
            &fabric,
            &desk.id(),
            group,
            GroupAudioSource::PickerToken(1),
            SESSION,
            now,
        )
        .unwrap();
    let admission = shelf.deliver(&fabric, &offers[0], "desk", now);
    desk.deliver(&fabric, &admission[0], "shelf", now);
    (fabric, group)
}

fn shelf_route() -> [u8; 16] {
    member_route_id(SESSION, &id("shelf"), output("shelf"))
}

/// Desk's view of its session, given what shelf's keepalives last reported
/// about shelf's clock of desk.
fn leader_status(
    desk: &Device,
    fabric: &DeviceFabric,
    shelf_reports: ClockSyncState,
) -> (SpeakerSessionStatus, usize) {
    let keepalive = PersonalFabricAction::RemoteAudioKeepalive {
        session_id: shelf_route(),
        authorization: [9; 16],
        playout: Some(shelf_reports),
    };
    assert!(desk.policy.permits_action(&keepalive));
    let view = desk.core.view(fabric, &desk.id(), &|route| {
        (route == shelf_route()).then_some(shelf_reports)
    });
    let session = &view.sessions[0];
    (session.status(), session.playing_members)
}

#[test]
fn with_media_on_and_display_input_off_the_group_syncs_its_clock_and_plays() {
    let now = Instant::now();
    let media_only = policy(true, false);
    let mut desk = Device::new("desk", media_only, now);
    let mut shelf = Device::new("shelf", media_only, now);
    let (fabric, _) = playing_pair(&mut desk, &mut shelf, now);

    // Shelf's inbound route holds desk's clock while it plays.
    shelf.clock.hold(desk.id(), now);
    let reported = shelf.clock.state(&desk.id(), now, 1_000_000);
    assert_eq!(reported, ClockSyncState::Syncing);
    assert_eq!(
        leader_status(&desk, &fabric, reported),
        (SpeakerSessionStatus::SyncingClocks, 1),
        "a member without the clock is shown syncing, not counted as playing"
    );

    // The clock round trip, admitted on both ends under Media alone. The
    // display-health probe would be refused here: that is not what the
    // clock rides on any more.
    assert!(
        !media_only.permits_action(&PersonalFabricAction::DeviceHealthProbe { nonce: [1; 16] })
    );
    let probes = shelf
        .clock
        .due_probes(|peer| peer == &id("desk"), now, 1_000_000, || [7; 16]);
    assert_eq!(probes.len(), 1);
    let (target, probe) = &probes[0];
    assert_eq!(target, &desk.id());
    assert!(desk.policy.permits_action(probe));
    let PersonalFabricAction::FabricClockProbe { nonce, sent_us } = *probe else {
        panic!("expected a clock probe, got {probe:?}");
    };
    let report = desk
        .clock
        .answer(nonce, sent_us, 9_000_400, 9_000_450)
        .expect("desk answers under Media");
    assert!(shelf.policy.permits_action(&report));
    let PersonalFabricAction::FabricClockReport { report } = report else {
        panic!("expected a clock report");
    };
    let arrived = now + Duration::from_millis(1);
    assert!(shelf.clock.accept(&desk.id(), report, arrived, 1_000_900));

    let reported = shelf.clock.state(&desk.id(), arrived, 1_000_900);
    assert_eq!(reported, ClockSyncState::Ready);
    assert!(shelf.clock.estimate(&desk.id(), 1_000_900).is_some());
    assert_eq!(
        leader_status(&desk, &fabric, reported),
        (SpeakerSessionStatus::Playing, 2),
        "both speakers play once shelf has desk's clock"
    );
    let member = shelf.core.view(&fabric, &shelf.id(), &|_| Some(reported));
    assert_eq!(member.sessions[0].status(), SpeakerSessionStatus::Playing);
}

#[test]
fn a_member_whose_clock_never_arrives_reports_why_after_the_deadline() {
    let now = Instant::now();
    let media_only = policy(true, false);
    let mut desk = Device::new("desk", media_only, now);
    let mut shelf = Device::new("shelf", media_only, now);
    let (fabric, _) = playing_pair(&mut desk, &mut shelf, now);
    shelf.clock.hold(desk.id(), now);
    assert_eq!(
        shelf
            .clock
            .due_probes(|_| true, now, 1_000_000, || [7; 16])
            .len(),
        1,
        "desk never answers"
    );
    let late = now + CLOCK_SYNC_DEADLINE;
    let reported = shelf.clock.state(&desk.id(), late, 6_000_000);
    assert_eq!(reported, ClockSyncState::Failed(ClockSyncFailure::NoReply));
    assert_eq!(
        leader_status(&desk, &fabric, reported),
        (
            SpeakerSessionStatus::ClockFailed(ClockSyncFailure::NoReply),
            1
        )
    );
}

#[test]
fn with_media_off_the_clock_and_the_group_refuse_with_a_reason() {
    let now = Instant::now();
    let media_on = policy(true, false);
    let mut desk = Device::new("desk", media_on, now);
    let mut shelf = Device::new("shelf", media_on, now);
    let (fabric, group) = playing_pair(&mut desk, &mut shelf, now);
    shelf.clock.hold(desk.id(), now);

    let media_off = policy(false, true);
    assert!(!media_off.permits_action(&probe()));
    let off = now + Duration::from_secs(1);
    shelf
        .clock
        .set_enabled(media_off.permits_action(&probe()), off);
    desk.clock
        .set_enabled(media_off.permits_action(&probe()), off);
    assert_eq!(
        shelf.clock.state(&desk.id(), off, 2_000_000),
        ClockSyncState::Failed(ClockSyncFailure::MediaDisabled)
    );
    assert!(
        shelf
            .clock
            .due_probes(|_| true, off, 2_000_000, || [7; 16])
            .is_empty()
    );
    assert!(desk.clock.answer([7; 16], 1, 2, 3).is_none());

    let teardown = desk.core.pump(&fabric, &desk.id(), false, off);
    assert!(
        teardown.iter().any(|item| item.target == shelf.id()),
        "turning Media off stops the member's session"
    );
    let refusal = desk
        .core
        .start_session(
            &fabric,
            &desk.id(),
            group,
            GroupAudioSource::PickerToken(1),
            [5; 16],
            off,
        )
        .expect_err("no session starts with Media off");
    assert_eq!(
        refusal,
        shell::shell::i18n::t("speaker-error-media-off"),
        "the refusal names the reason"
    );
    assert_ne!(refusal, "speaker-error-media-off");
}
