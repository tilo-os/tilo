use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use media::sync_playout::ChannelRole;

use super::*;
use tilo_network_actor::fabric::speaker_group::{RoomPoint, StereoSide};
use tilo_network_actor::fabric::{
    DeviceAbout, DeviceKind, FabricCapabilities, PersonDevice, PersonDevices,
};
use tilo_resource::Replica;
use tilo_workspace_service::workspaces::person_workspace::devices::Devices;

const LEFT: SpeakerRole = SpeakerRole::Stereo(StereoSide::Left);
const RIGHT: SpeakerRole = SpeakerRole::Stereo(StereoSide::Right);

/// The 32 bytes in hex that name `device` in the record of the person's devices.
fn key(device: &str) -> String {
    let mut raw = [0; 32];
    let bytes = device.as_bytes();
    raw[..bytes.len()].copy_from_slice(bytes);
    hex::encode(raw)
}

fn id(value: &str) -> DeviceId {
    DeviceId::new(key(value)).unwrap()
}

fn output(device: &str) -> EndpointSelector {
    EndpointSelector::mint(&id(device), "alsa_output.analog-stereo")
}

fn microphone(device: &str) -> EndpointSelector {
    EndpointSelector::mint(&id(device), "alsa_input.analog-stereo")
}

fn group_id(value: u8) -> SpeakerGroupId {
    SpeakerGroupId([value; 16])
}

/// The person's devices: the record of their devices and what each says live.
struct World {
    record: Replica<Devices>,
    fabric: PersonDevices,
}

impl World {
    fn new() -> Self {
        Self {
            record: Replica::new(),
            fabric: PersonDevices::default(),
        }
    }

    /// `device` joins the person's devices, offering one output and one microphone.
    fn device(&mut self, device: &str, audio_group: bool) {
        let local = id(device);
        self.record
            .apply(Devices::KEEP_DEVICE.with((key(device), "00".to_owned())))
            .expect("the device is kept");
        let endpoints = vec![
            tilo_workspace_service::workspaces::person_workspace::speaker::SpeakerEndpoint {
                selector: output(device).to_hex(),
                microphone: false,
                label: format!("{device} speakers"),
                is_default: true,
            },
            tilo_workspace_service::workspaces::person_workspace::speaker::SpeakerEndpoint {
                selector: microphone(device).to_hex(),
                microphone: true,
                label: format!("{device} microphone"),
                is_default: true,
            },
        ];
        self.record
            .apply(Devices::SAY_SPEAKER_ENDPOINTS.with((
                key(device),
                hex::encode(postcard::to_allocvec(&endpoints).expect("endpoints encode")),
            )))
            .expect("the device offers its endpoints");
        let mut known = PersonDevice::new(
            local,
            DeviceAbout {
                label: device.into(),
                kind: DeviceKind::Speaker,
                capabilities: FabricCapabilities {
                    audio_output: true,
                    audio_group,
                    ..FabricCapabilities::default()
                },
            },
        );
        known.online = true;
        known.last_seen_unix_ms = 1;
        self.fabric.insert(known);
    }

    fn state(&self) -> SpeakerState {
        SpeakerState::of(&self.record.state().expect("the record reads"))
    }

    fn apply(&mut self, writes: Vec<SpeakerWrite>) {
        for write in writes {
            self.record
                .apply(action_of(write).expect("what a write carries encodes"))
                .expect("a speaker-group write holds");
        }
    }
}

fn members(pairs: &[(&str, SpeakerRole)]) -> Vec<MemberSpec> {
    pairs
        .iter()
        .map(|(device, role)| MemberSpec {
            device: id(device),
            output: output(device),
            role: *role,
        })
        .collect()
}

/// A pair the office leads, the shelf consenting to play its right side.
fn pair(world: &mut World, core: &mut SpeakerGroupCore, group: SpeakerGroupId) {
    let office = id("office");
    let writes = core
        .create_group(
            &world.state(),
            &world.fabric,
            &office,
            group,
            "Living room",
            &members(&[("office", LEFT), ("shelf", RIGHT)]),
            Instant::now(),
        )
        .expect("the office leads the pair");
    world.apply(writes);
}

/// The shelf answers the consent prompt `core` now holds for `group`.
fn consent(world: &mut World, core: &mut SpeakerGroupCore, group: SpeakerGroupId, approved: bool) {
    let shelf = id("shelf");
    let writes = core
        .resolve_consent(&world.state(), &shelf, group, approved, Instant::now())
        .expect("the shelf was asked");
    world.apply(writes);
}

fn reconcile(world: &World, core: &mut SpeakerGroupCore, device: &str) -> Vec<SpeakerWrite> {
    core.reconcile(
        &world.state(),
        &world.fabric,
        &id(device),
        true,
        Instant::now(),
        &mut || [9; 16],
    )
}

#[test]
fn a_group_is_written_to_the_record_and_a_member_answers_once() {
    let mut world = World::new();
    world.device("office", true);
    world.device("shelf", true);
    let mut office = SpeakerGroupCore::default();
    let group = group_id(1);
    pair(&mut world, &mut office, group);

    let written = world.state();
    let led = written.group(group).expect("the office leads the pair");
    assert_eq!(led.leader, id("office"));
    assert_eq!(led.members.len(), 2);
    assert!(
        written.memberships_of(&id("shelf")).is_empty(),
        "the shelf has not agreed yet"
    );
    assert_eq!(
        written.member_states(&world.fabric, led),
        vec![
            SpeakerMemberStatus::Ready,
            SpeakerMemberStatus::AwaitingConsent
        ],
        "the shelf is asked"
    );

    // The shelf's own pass asks its person once; answering yes writes its consent.
    let mut shelf = SpeakerGroupCore::default();
    reconcile(&world, &mut shelf, "shelf");
    let pending = shelf.view(&world.state(), &world.fabric, &id("shelf"), &|_| None);
    assert_eq!(pending.consents.len(), 1, "the shelf asks its person once");
    assert!(!pending.consents[0].prompt().is_empty());
    consent(&mut world, &mut shelf, group, true);
    let written = world.state();
    let led = written.group(group).unwrap();
    assert_eq!(
        written.member_states(&world.fabric, led),
        vec![SpeakerMemberStatus::Ready, SpeakerMemberStatus::Ready]
    );

    // A second pass asks nothing more.
    reconcile(&world, &mut shelf, "shelf");
    let view = shelf.view(&world.state(), &world.fabric, &id("shelf"), &|_| None);
    assert!(view.consents.is_empty());
    assert_eq!(view.memberships.len(), 1);
}

#[test]
fn a_consented_group_admits_playbacks_from_its_source_only() {
    let mut world = World::new();
    world.device("office", true);
    world.device("shelf", true);
    let mut office = SpeakerGroupCore::default();
    let mut shelf = SpeakerGroupCore::default();
    let group = group_id(2);
    pair(&mut world, &mut office, group);
    reconcile(&world, &mut shelf, "shelf");
    consent(&mut world, &mut shelf, group, true);

    // The office plays to the pair: one offer per remote member, the shelf's right output.
    let session = [4; 16];
    let writes = office
        .start_session(
            &world.state(),
            &world.fabric,
            &id("office"),
            group,
            GroupAudioSource::System,
            session,
            Instant::now(),
        )
        .expect("the office plays");
    let offered = writes
        .iter()
        .filter_map(|write| match write {
            SpeakerWrite::Offer { route, target, .. } => Some((*route, target.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(offered.len(), 1, "the office plays on its own output too");
    assert_eq!(offered[0].1, id("shelf"));
    assert_eq!(
        offered[0].0,
        member_route_id(session, &id("shelf"), output("shelf"))
    );
    world.apply(writes);

    // The shelf admits it with a capability of its own, bound to that exact source.
    let writes = reconcile(&world, &mut shelf, "shelf");
    let route = offered[0].0;
    assert!(
        writes.iter().any(|write| matches!(
            write,
            SpeakerWrite::Admit { route: admitted, authorization: Some(value) }
                if *admitted == route && *value == [9; 16]
        )),
        "the shelf admits the playback with its capability: {writes:?}"
    );
    world.apply(writes);
    let recorded = world.state();
    let playback = recorded
        .playback(route)
        .expect("the playback is in the record");
    assert!(playback.admitted);
    assert_eq!(playback.authorization, Some([9; 16]));
    assert_eq!(playback.source, id("office"));

    // The office reads the admission and starts the route to the shelf.
    reconcile(&world, &mut office, "office");
    let plans = office.playback_plans();
    assert_eq!(plans.len(), 1);
    let shelf_member = plans[0]
        .members
        .iter()
        .find(|member| member.device == id("shelf"))
        .expect("the shelf plays");
    assert_eq!(shelf_member.authorization, Some([9; 16]));
    assert_eq!(shelf_member.channel, ChannelRole::Right);
    let receiving = shelf
        .route_specs(&world.state(), &id("shelf"))
        .into_iter()
        .find(|spec| matches!(spec, RouteSpec::Receive { .. }))
        .expect("the shelf receives what the office plays");
    let RouteSpec::Receive {
        route: received,
        authorization,
        output: on,
        channel,
        ..
    } = receiving
    else {
        panic!("the shelf receives what the office plays");
    };
    assert_eq!(received, route);
    assert_eq!(authorization, [9; 16]);
    assert_eq!(on, output("shelf"));
    assert_eq!(channel, ChannelRole::Right);
    assert!(shelf.admits(route, &id("office")));

    // A playback from a device that is not the source is not what the shelf admitted.
    assert!(!shelf.admits(route, &id("office2")));
    assert!(!shelf.admits([7; 16], &id("office")));

    // The office ends it: the record loses the playback, and the shelf stops its route.
    let writes = office.stop_session(session).expect("the office stops");
    world.apply(writes);
    assert!(world.state().playback(route).is_none());
    let writes = reconcile(&world, &mut shelf, "shelf");
    assert!(
        writes.iter().any(
            |write| matches!(write, SpeakerWrite::Stop { route: stopped, .. } if *stopped == route)
        ),
        "the shelf lets go: {writes:?}"
    );
    world.apply(writes);
    assert!(shelf.route_specs(&world.state(), &id("shelf")).is_empty());
}

#[test]
fn consent_ends_when_the_group_changes_under_a_member() {
    let mut world = World::new();
    world.device("office", true);
    world.device("shelf", true);
    let mut office = SpeakerGroupCore::default();
    let mut shelf = SpeakerGroupCore::default();
    let group = group_id(3);
    pair(&mut world, &mut office, group);
    reconcile(&world, &mut shelf, "shelf");
    consent(&mut world, &mut shelf, group, true);

    // The office changes the shelf's role: its consent no longer covers the member, and the
    // shelf is asked again rather than playing a role it did not agree to.
    let writes = office
        .update_group(
            &world.state(),
            &world.fabric,
            &id("office"),
            group,
            "Living room",
            &members(&[("office", LEFT), ("shelf", RIGHT)]),
            Instant::now(),
        )
        .expect("the office edits the group");
    world.apply(writes);
    // A consent for the same output and role still covers it.
    reconcile(&world, &mut shelf, "shelf");
    let view = shelf.view(&world.state(), &world.fabric, &id("shelf"), &|_| None);
    assert!(view.consents.is_empty(), "the same role stays agreed");
    assert_eq!(view.memberships.len(), 1);

    // Moving the shelf to the left side is a different contribution: it is asked again, and
    // until it agrees the member reads as awaiting consent.
    let writes = office
        .update_group(
            &world.state(),
            &world.fabric,
            &id("office"),
            group,
            "Living room",
            &members(&[("office", RIGHT), ("shelf", LEFT)]),
            Instant::now(),
        )
        .expect("the office swaps the sides");
    world.apply(writes);
    let state = world.state();
    let led = state.group(group).unwrap();
    assert_eq!(
        state.member_states(&world.fabric, led),
        vec![
            SpeakerMemberStatus::Ready,
            SpeakerMemberStatus::AwaitingConsent
        ]
    );
    reconcile(&world, &mut shelf, "shelf");
    let view = shelf.view(&world.state(), &world.fabric, &id("shelf"), &|_| None);
    assert_eq!(view.consents.len(), 1, "the shelf agrees to the new role");

    // And the consent it gave for the old role is withdrawn by its next pass.
    let writes = reconcile(&world, &mut shelf, "shelf");
    assert!(
        writes
            .iter()
            .any(|write| matches!(write, SpeakerWrite::Withdraw { play: true, .. })),
        "a consent the leader no longer writes as agreed goes: {writes:?}"
    );
    world.apply(writes);
    assert!(world.state().memberships_of(&id("shelf")).is_empty());
}

#[test]
fn a_groups_teardown_releases_its_members() {
    let mut world = World::new();
    world.device("office", true);
    world.device("shelf", true);
    let mut office = SpeakerGroupCore::default();
    let mut shelf = SpeakerGroupCore::default();
    let group = group_id(4);
    pair(&mut world, &mut office, group);
    reconcile(&world, &mut shelf, "shelf");
    consent(&mut world, &mut shelf, group, true);
    let session = [6; 16];
    let writes = office
        .start_session(
            &world.state(),
            &world.fabric,
            &id("office"),
            group,
            GroupAudioSource::System,
            session,
            Instant::now(),
        )
        .expect("the office plays");
    world.apply(writes);
    let writes = reconcile(&world, &mut shelf, "shelf");
    world.apply(writes);
    reconcile(&world, &mut office, "office");

    // The office deletes the group: its members are let go and the playback ends.
    let writes = office.delete_group(&id("office"), group);
    assert!(
        writes
            .iter()
            .any(|write| matches!(write, SpeakerWrite::Stop { .. })),
        "the playback ends: {writes:?}"
    );
    world.apply(writes);
    let state = world.state();
    assert!(state.group(group).is_none());
    assert!(state.playbacks.is_empty());
    assert!(
        office
            .view(&state, &world.fabric, &id("office"), &|_| None)
            .groups
            .is_empty()
    );

    // The shelf's next pass hears nothing of the group: its consent goes, and its route ends.
    let writes = reconcile(&world, &mut shelf, "shelf");
    assert!(
        writes
            .iter()
            .any(|write| matches!(write, SpeakerWrite::Withdraw { play: true, .. })),
        "the shelf drops its consent: {writes:?}"
    );
    world.apply(writes);
    assert!(world.state().memberships_of(&id("shelf")).is_empty());
}

#[test]
fn admitted_playbacks_become_routes_by_endpoint_selector() {
    let mut world = World::new();
    world.device("office", true);
    world.device("shelf", true);
    let mut office = SpeakerGroupCore::default();
    let mut shelf = SpeakerGroupCore::default();
    let group = group_id(5);
    pair(&mut world, &mut office, group);
    reconcile(&world, &mut shelf, "shelf");
    consent(&mut world, &mut shelf, group, true);

    let session = [8; 16];
    world.apply(
        office
            .start_session(
                &world.state(),
                &world.fabric,
                &id("office"),
                group,
                GroupAudioSource::System,
                session,
                Instant::now(),
            )
            .unwrap(),
    );
    let writes = reconcile(&world, &mut shelf, "shelf");
    world.apply(writes);
    reconcile(&world, &mut office, "office");

    // The office plays the left side on its own output and sends the right side to the shelf.
    let playing = office.route_specs(&world.state(), &id("office"));
    let local = playing
        .iter()
        .find(|spec| matches!(spec, RouteSpec::Local { .. }))
        .expect("the office plays its own side");
    let RouteSpec::Local {
        output: on,
        channel,
        ..
    } = local
    else {
        panic!("the office plays its own side");
    };
    assert_eq!(*on, output("office"));
    assert_eq!(*channel, ChannelRole::Left);
    assert!(
        playing
            .iter()
            .any(|spec| matches!(spec, RouteSpec::Send { target, .. } if *target == id("shelf")))
    );

    // And the recorded route changes only when the sessions it stands for do.
    let running = playing
        .iter()
        .map(|spec| (spec.route(), spec.clone()))
        .collect::<BTreeMap<_, _>>();
    let (end, start) = route_changes(&running, &playing);
    assert!(end.is_empty() && start.is_empty());

    // A member that leaves the group ends the route: the shelf withdraws its consent.
    let writes = shelf
        .leave(&world.state(), &id("shelf"), group)
        .expect("the shelf leaves");
    world.apply(writes);
    let writes = reconcile(&world, &mut office, "office");
    assert!(
        writes
            .iter()
            .any(|write| matches!(write, SpeakerWrite::Stop { .. })),
        "the office stops sending to the shelf: {writes:?}"
    );
    world.apply(writes);
    let playing = office.route_specs(&world.state(), &id("office"));
    assert!(
        !playing
            .iter()
            .any(|spec| matches!(spec, RouteSpec::Send { .. }))
    );
}

#[test]
fn a_groups_microphones_listen_only_for_the_leader_and_with_consent() {
    let mut world = World::new();
    world.device("office", true);
    world.device("shelf", true);
    let mut office = SpeakerGroupCore::default();
    let mut shelf = SpeakerGroupCore::default();
    let group = group_id(6);
    pair(&mut world, &mut office, group);
    reconcile(&world, &mut shelf, "shelf");
    consent(&mut world, &mut shelf, group, true);

    let published = world.state().group(group).unwrap().clone();
    let candidates = office.listener_candidates(
        &world.state(),
        &world.fabric,
        &id("office"),
        &published,
        Instant::now(),
    );
    let shelf_candidate = candidates
        .iter()
        .find(|candidate| candidate.device == id("shelf"))
        .expect("the shelf has a microphone");
    assert_eq!(shelf_candidate.state, ListenerState::NotAsked);
    assert_eq!(shelf_candidate.microphone, microphone("shelf"));

    // The office asks the shelf to let it listen; the shelf's person says no.
    let asks = office.invite_listeners(
        &world.state(),
        &published,
        &[(id("shelf"), microphone("shelf"))],
        Instant::now(),
    );
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].target, id("shelf"));
    shelf
        .handle_listen_action(
            &world.state(),
            &id("shelf"),
            &id("office"),
            &asks[0].action,
            Instant::now(),
        )
        .expect("the shelf takes the invite");
    assert!(!shelf.may_listen(
        &world.state(),
        &world.fabric,
        &id("shelf"),
        &id("office"),
        group,
        microphone("shelf")
    ));
    let writes = shelf
        .resolve_listen_consent(&world.state(), &id("shelf"), group, false, Instant::now())
        .expect("the shelf answers");
    world.apply(writes);
    assert!(world.state().listeners_of(&id("shelf")).is_empty());

    // Asked again, it agrees, and only the leader may then record with it.
    let asks = office.invite_listeners(
        &world.state(),
        &published,
        &[(id("shelf"), microphone("shelf"))],
        Instant::now() + super::CONSENT_TTL + Duration::from_secs(1),
    );
    shelf
        .handle_listen_action(
            &world.state(),
            &id("shelf"),
            &id("office"),
            &asks[0].action,
            Instant::now(),
        )
        .unwrap();
    let writes = shelf
        .resolve_listen_consent(&world.state(), &id("shelf"), group, true, Instant::now())
        .expect("the shelf agrees");
    world.apply(writes);
    assert!(shelf.may_listen(
        &world.state(),
        &world.fabric,
        &id("shelf"),
        &id("office"),
        group,
        microphone("shelf")
    ));
    assert!(
        !shelf.may_listen(
            &world.state(),
            &world.fabric,
            &id("shelf"),
            &id("other"),
            group,
            microphone("shelf")
        ),
        "only the group's leader may listen"
    );

    // Stopping it takes the consent back from the record.
    let writes = shelf
        .stop_listening(&world.state(), &id("shelf"), group)
        .expect("the shelf stops listening");
    world.apply(writes);
    assert!(world.state().listeners_of(&id("shelf")).is_empty());
}

/// The route id shelf's audio travels under within `session`.
fn shelf_route(session: [u8; 16]) -> [u8; 16] {
    member_route_id(session, &id("shelf"), output("shelf"))
}

#[test]
fn a_member_route_is_one_per_session_and_member() {
    let session = [3; 16];
    assert_eq!(shelf_route(session), shelf_route(session));
    assert_ne!(shelf_route(session), shelf_route([4; 16]));
    assert_ne!(
        shelf_route(session),
        member_route_id(session, &id("office"), output("shelf"))
    );
    assert_ne!(shelf_route(session), [0; 16]);
}

#[test]
fn silence_is_not_played() {
    // A session with no ready member is refused rather than played to nobody.
    let mut world = World::new();
    world.device("office", true);
    world.device("shelf", true);
    let shelf = id("shelf");
    let mut offline = world
        .fabric
        .get(&shelf)
        .cloned()
        .expect("the shelf is published");
    offline.online = false;
    world.fabric.insert(offline);
    let mut office = SpeakerGroupCore::default();
    let group = group_id(7);
    let writes = office
        .create_group(
            &world.state(),
            &world.fabric,
            &id("office"),
            group,
            "Shelf alone",
            &members(&[(
                "shelf",
                SpeakerRole::RoomPoint(RoomPoint {
                    x_mm: 0,
                    y_mm: 0,
                    z_mm: 0,
                }),
            )]),
            Instant::now(),
        )
        .expect("the office leads a group on the shelf alone");
    world.apply(writes);
    let started = office.start_session(
        &world.state(),
        &world.fabric,
        &id("office"),
        group,
        GroupAudioSource::System,
        [1; 16],
        Instant::now(),
    );
    assert!(
        started.is_err(),
        "a group nobody can play is not played: {started:?}"
    );
}
