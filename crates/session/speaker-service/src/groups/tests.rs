use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use media::sync_playout::ChannelRole;

use super::*;
use tilo_network_actor::fabric::speaker_group::{StereoSide, SurroundChannel};
use tilo_network_actor::fabric::{
    DeviceAbout, DeviceKind, FabricCapabilities, PersonDevice, PersonDevices,
};

const LEFT: SpeakerRole = SpeakerRole::Stereo(StereoSide::Left);
const RIGHT: SpeakerRole = SpeakerRole::Stereo(StereoSide::Right);

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

/// One simulated device: its policy core and what it last published.
struct Device {
    name: &'static str,
    core: SpeakerGroupCore,
    counter: u64,
    online: bool,
    audio_group: bool,
}

impl Device {
    fn new(name: &'static str) -> Self {
        let mut core = SpeakerGroupCore::default();
        core.seed(None);
        Self {
            name,
            core,
            counter: 0,
            online: true,
            audio_group: true,
        }
    }

    fn id(&self) -> DeviceId {
        id(self.name)
    }

    /// Keep what this device says of itself into `fabric`, replacing what it
    /// said before.
    fn publish(&mut self, fabric: &mut PersonDevices) {
        self.counter = self.counter.saturating_add(1);
        let endpoints = vec![
            AudioEndpoint {
                selector: output(self.name),
                kind: AudioEndpointKind::Output,
                label: format!("{} speakers", self.name),
                is_default: true,
            },
            AudioEndpoint {
                selector: microphone(self.name),
                kind: AudioEndpointKind::Microphone,
                label: format!("{} microphone", self.name),
                is_default: true,
            },
        ];
        let mut device = PersonDevice::new(
            self.id(),
            DeviceAbout {
                label: self.name.into(),
                kind: DeviceKind::Speaker,
                capabilities: FabricCapabilities {
                    audio_output: true,
                    audio_group: self.audio_group,
                    ..FabricCapabilities::default()
                },
            },
        );
        device.online = self.online;
        device.last_seen_unix_ms = self.counter;
        device.speakers = self.core.authored(endpoints, &stamp(self.name));
        fabric.insert(device);
    }

    fn handle(
        &mut self,
        fabric: &PersonDevices,
        item: &Egress,
        from: &str,
        now: Instant,
    ) -> Vec<Egress> {
        assert_eq!(
            item.target,
            self.id(),
            "{item:?} delivered to the wrong device"
        );
        let mut mint = || [9; 16];
        self.core
            .handle(fabric, &self.id(), &id(from), &item.action, now, &mut mint)
            .expect("a speaker-group action")
    }
}

/// The route id shelf's audio travels under within `session`.
fn shelf_route(session: [u8; 16]) -> [u8; 16] {
    member_route_id(session, &id("shelf"), output("shelf"))
}

fn pair_members() -> Vec<MemberSpec> {
    vec![
        MemberSpec {
            device: id("office"),
            output: output("office"),
            role: LEFT,
        },
        MemberSpec {
            device: id("shelf"),
            output: output("shelf"),
            role: RIGHT,
        },
    ]
}

/// Office leads a stereo pair with shelf; returns the fabric with both
/// published and the group id, before shelf has answered.
fn invited_pair(
    office: &mut Device,
    shelf: &mut Device,
    now: Instant,
) -> (PersonDevices, SpeakerGroupId, Vec<Egress>) {
    let mut fabric = PersonDevices::default();
    office.publish(&mut fabric);
    shelf.publish(&mut fabric);
    let group = SpeakerGroupId([4; 16]);
    let invites = office
        .core
        .create_group(
            &fabric,
            &office.id(),
            group,
            "Living room",
            &pair_members(),
            now,
        )
        .unwrap();
    office.publish(&mut fabric);
    (fabric, group, invites)
}

/// Office and shelf form a consented stereo pair.
fn joined_pair(
    office: &mut Device,
    shelf: &mut Device,
    now: Instant,
) -> (PersonDevices, SpeakerGroupId) {
    let (mut fabric, group, invites) = invited_pair(office, shelf, now);
    assert_eq!(invites.len(), 1);
    assert!(shelf.handle(&fabric, &invites[0], "office", now).is_empty());
    let decision = shelf
        .core
        .resolve_consent(&fabric, &shelf.id(), group, true, &stamp("shelf"))
        .unwrap();
    shelf.publish(&mut fabric);
    assert!(
        office
            .handle(&fabric, &decision[0], "shelf", now)
            .is_empty()
    );
    (fabric, group)
}

fn admitted_session(
    office: &mut Device,
    shelf: &mut Device,
    fabric: &PersonDevices,
    group: SpeakerGroupId,
    session: [u8; 16],
    now: Instant,
) -> [u8; 16] {
    let offers = office
        .core
        .start_session(
            fabric,
            &office.id(),
            group,
            GroupAudioSource::PickerToken(1),
            session,
            now,
        )
        .unwrap();
    let decision = shelf.handle(fabric, &offers[0], "office", now);
    office.handle(fabric, &decision[0], "shelf", now);
    assert_eq!(plan_of(office, session).members.len(), 2);
    shelf_route(session)
}

/// The stop `device` is sent for `session`.
fn stop_to(device: &DeviceId, session: [u8; 16], authorization: Option<[u8; 16]>) -> Egress {
    egress(
        device,
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: session,
            authorization,
        },
    )
}

/// The leave `device` is sent for `group`.
fn leave_to(device: &DeviceId, group: SpeakerGroupId) -> Egress {
    egress(device, PersonalFabricAction::SpeakerGroupLeave { group })
}

/// The leader's plan for `session`.
fn plan_of(office: &Device, session: [u8; 16]) -> GroupPlaybackPlan {
    office
        .core
        .playback_plans()
        .into_iter()
        .find(|plan| plan.session_id == session)
        .expect("the session's plan")
}

tilo_test_env::scenario! {
    fn a_group_invites_its_remote_members_and_a_member_answers_once;

    phase a_new_group_asks_only_remote_members_and_their_answer_makes_them_ready;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (mut fabric, group, invites) = invited_pair(&mut office, &mut shelf, now);
        assert_eq!(
            invites,
            vec![egress(
                &id("shelf"),
                PersonalFabricAction::SpeakerGroupInvite {
                    group,
                    name: "Living room".into(),
                    output: output("shelf"),
                    role: RIGHT,
                },
            )]
        );
        assert_eq!(
            fabric.speaker_member_states(fabric.speaker_group(group).unwrap()),
            vec![
                SpeakerMemberStatus::Ready,
                SpeakerMemberStatus::AwaitingConsent
            ]
        );

        // A forged invite the leader never published cannot be approved.
        let forged = egress(
            &id("shelf"),
            PersonalFabricAction::SpeakerGroupInvite {
                group,
                name: "Living room".into(),
                output: output("shelf"),
                role: LEFT,
            },
        );
        shelf.handle(&fabric, &forged, "office", now);
        assert!(
            shelf
                .core
                .view(&fabric, &shelf.id(), &|_| None)
                .consents
                .is_empty()
        );
        assert!(
            shelf
                .core
                .resolve_consent(&fabric, &shelf.id(), group, true, &stamp("shelf"))
                .is_err()
        );
        assert!(shelf.core.memberships.iter().flatten().next().is_none());

        // The real invite is one prompt that says what it is for.
        shelf.handle(&fabric, &invites[0], "office", now);
        let view = shelf.core.view(&fabric, &shelf.id(), &|_| None);
        assert_eq!(view.consents.len(), 1);
        let consent = &view.consents[0];
        assert_eq!(consent.name, "Living room");
        assert_eq!(consent.leader_label, "office");
        assert_eq!(consent.scope, SpeakerConsentScope::Play(RIGHT));
        assert_eq!(consent.endpoint_label.as_deref(), Some("shelf speakers"));
        assert_eq!(consent.prompt(), "Join Living room as the right speaker?");

        let decision = shelf
            .core
            .resolve_consent(&fabric, &shelf.id(), group, true, &stamp("shelf"))
            .unwrap();
        shelf.publish(&mut fabric);
        assert!(
            office
                .handle(&fabric, &decision[0], "shelf", now)
                .is_empty()
        );
        assert_eq!(
            fabric.speaker_member_states(fabric.speaker_group(group).unwrap()),
            vec![SpeakerMemberStatus::Ready, SpeakerMemberStatus::Ready]
        );
        assert!(
            shelf
                .core
                .view(&fabric, &shelf.id(), &|_| None)
                .consents
                .is_empty()
        );
        // A consented member is not asked again.
        assert!(
            office
                .core
                .pump(&fabric, &office.id(), true, now + INVITE_RETRY)
                .is_empty()
        );

        // A calibration trim keeps the consent and must stay in range.
        let trim = SpeakerTrim {
            delay_ns: 2_500_000,
            gain_mb: -150,
            eq: Vec::new(),
        };
        office
            .core
            .set_trims(group, &[(shelf.id(), output("shelf"), trim.clone())])
            .unwrap();
        office.publish(&mut fabric);
        let published = fabric.speaker_group(group).unwrap();
        assert_eq!(published.member(&shelf.id()).unwrap().trim, trim);
        assert_eq!(
            fabric.speaker_member_states(published),
            vec![SpeakerMemberStatus::Ready, SpeakerMemberStatus::Ready]
        );
        assert!(
            office
                .core
                .set_trims(
                    group,
                    &[(
                        shelf.id(),
                        output("shelf"),
                        SpeakerTrim {
                            delay_ns: u64::MAX,
                            ..SpeakerTrim::default()
                        }
                    )]
                )
                .is_err()
        );
    }

    phase an_unanswered_invite_is_repeated_while_the_member_is_online;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (fabric, group, invites) = invited_pair(&mut office, &mut shelf, now);
        assert!(office.core.pump(&fabric, &office.id(), true, now).is_empty());
        assert_eq!(
            office.core
                .pump(&fabric, &office.id(), true, now + INVITE_RETRY)
                .len(),
            1
        );

        // A stale prompt expires, and cannot be approved afterwards.
        shelf.handle(&fabric, &invites[0], "office", now);
        shelf.core.pump(
            &fabric,
            &shelf.id(),
            true,
            now + CONSENT_TTL + Duration::from_secs(1),
        );
        assert!(
            shelf
                .core
                .resolve_consent(&fabric, &shelf.id(), group, true, &stamp("shelf"))
                .is_err()
        );
    }

    phase declining_stops_the_leader_asking_until_the_group_is_edited;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (fabric, group, invites) = invited_pair(&mut office, &mut shelf, now);
        shelf.handle(&fabric, &invites[0], "office", now);
        let decision = shelf
            .core
            .resolve_consent(&fabric, &shelf.id(), group, false, &stamp("shelf"))
            .unwrap();
        office.handle(&fabric, &decision[0], "shelf", now);
        assert!(
            office
                .core
                .pump(&fabric, &office.id(), true, now + INVITE_RETRY * 3)
                .is_empty()
        );
        let edited = office
            .core
            .update_group(&fabric, &office.id(), group, "Lounge", &pair_members(), now)
            .unwrap();
        assert!(
            edited
                .iter()
                .any(|item| matches!(item.action, PersonalFabricAction::SpeakerGroupInvite { .. }))
        );
    }
}

tilo_test_env::scenario! {
    fn a_consented_group_admits_sessions_from_its_source_only;

    phase any_of_your_devices_plays_and_admits_and_only_the_source_speaks_for_a_session;
    {
        let now = Instant::now();
        let (mut office, mut shelf, mut phone) = (
            Device::new("office"),
            Device::new("shelf"),
            Device::new("phone"),
        );
        let (mut fabric, group) = joined_pair(&mut office, &mut shelf, now);
        phone.publish(&mut fabric);

        // Any of the person's devices can play to the group.
        let session = [3; 16];
        let offers = phone
            .core
            .start_session(
                &fabric,
                &phone.id(),
                group,
                GroupAudioSource::PickerToken(11),
                session,
                now,
            )
            .unwrap();
        assert_eq!(offers.len(), 2);
        for offer in &offers {
            let (member, from) = if offer.target == office.id() {
                (&mut office, "office")
            } else {
                (&mut shelf, "shelf")
            };
            let decision = member.handle(&fabric, offer, "phone", now);
            assert_eq!(decision.len(), 1);
            assert_eq!(decision[0].target, phone.id());
            assert!(matches!(
                decision[0].action,
                PersonalFabricAction::SpeakerGroupSessionDecision {
                    authorization: Some(value),
                    ..
                } if value == [9; 16]
            ));
            assert!(phone.handle(&fabric, &decision[0], from, now).is_empty());
        }
        let plans = phone.core.playback_plans();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].source, GroupAudioSource::PickerToken(11));
        let channels = plans[0]
            .members
            .iter()
            .map(|member| {
                (
                    member.device.as_str().to_string(),
                    member.channel,
                    member.authorization,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            channels,
            vec![
                ("office".into(), ChannelRole::Left, Some([9; 16])),
                ("shelf".into(), ChannelRole::Right, Some([9; 16])),
            ]
        );
        let grant = shelf
            .core
            .grant(&fabric, &shelf.id(), shelf_route(session), &phone.id())
            .unwrap();
        assert_eq!(grant.channel, ChannelRole::Right);
        assert_eq!(grant.output, output("shelf"));
        assert!(
            shelf
                .core
                .grant(&fabric, &shelf.id(), shelf_route(session), &office.id())
                .is_none()
        );

        let member_route = shelf_route(session);

        // The leader plays its own output locally, without asking itself.
        let movie = [5; 16];
        let offers = office
            .core
            .start_session(
                &fabric,
                &office.id(),
                group,
                GroupAudioSource::MediaSession("movie".into()),
                movie,
                now,
            )
            .unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].target, shelf.id());
        let plan = plan_of(&office, movie);
        assert_eq!(plan.members.len(), 1, "shelf has not admitted yet");
        assert_eq!(plan.members[0].device, office.id());
        assert_eq!(plan.members[0].authorization, None);

        // A member without consent refuses a session.
        let unconsented = egress(
            &shelf.id(),
            PersonalFabricAction::SpeakerGroupSessionOffer {
                session_id: [6; 16],
                group: SpeakerGroupId([6; 16]),
            },
        );
        let reply = shelf.handle(&fabric, &unconsented, "office", now);
        assert_eq!(
            reply,
            vec![egress(
                &office.id(),
                PersonalFabricAction::SpeakerGroupSessionDecision {
                    session_id: [6; 16],
                    group: SpeakerGroupId([6; 16]),
                    authorization: None,
                },
            )]
        );
        assert!(!shelf.core.inbound.contains_key(&[6; 16]));

        // A session stop from anyone but its source is ignored.
        let stop = stop_to(&shelf.id(), member_route, Some([9; 16]));
        shelf.handle(&fabric, &stop, "office", now);
        assert!(shelf.core.inbound.contains_key(&member_route));
        shelf.handle(&fabric, &stop, "phone", now);
        assert!(!shelf.core.inbound.contains_key(&member_route));
    }

    phase an_ended_route_or_session_stops_playing_and_a_source_going_offline_ends_it;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (fabric, group) = joined_pair(&mut office, &mut shelf, now);
        let session = [8; 16];
        let route = admitted_session(&mut office, &mut shelf, &fabric, group, session, now);
        // Admitted sessions become send, local and receive routes.
        let leader = office.core.route_specs(&fabric, &office.id());
        assert_eq!(leader.len(), 2);
        assert!(leader.iter().any(|spec| matches!(
            spec,
            RouteSpec::Local { channel: ChannelRole::Left, output: out, .. } if *out == output("office")
        )));
        assert!(leader.contains(&RouteSpec::Send {
            route,
            target: shelf.id(),
            authorization: [9; 16],
            source: GroupAudioSource::PickerToken(1),
        }));
        let member = shelf.core.route_specs(&fabric, &shelf.id());
        assert_eq!(
            member,
            vec![RouteSpec::Receive {
                route,
                source: office.id(),
                authorization: [9; 16],
                output: output("shelf"),
                channel: ChannelRole::Right,
                trim: SpeakerTrim::default(),
            }]
        );
        assert!(shelf.core.admits(route, &office.id()));
        assert!(!shelf.core.admits(route, &id("phone")));

        // A route the audio path lost ends its member session.
        assert_eq!(
            shelf.core.route_lost(route),
            vec![stop_to(&office.id(), route, Some([9; 16]))]
        );
        assert_eq!(
            office.core.route_lost(route),
            vec![stop_to(&shelf.id(), route, Some([9; 16]))]
        );
        let plan = plan_of(&office, session);
        assert_eq!(plan.members.len(), 1);

        // A late admission for an ended session is revoked.
        let late = egress(
            &office.id(),
            PersonalFabricAction::SpeakerGroupSessionDecision {
                session_id: [1; 16],
                group,
                authorization: Some([7; 16]),
            },
        );
        assert_eq!(
            office.handle(&fabric, &late, "shelf", now),
            vec![stop_to(&shelf.id(), [1; 16], Some([7; 16]))]
        );

        // A member that never answers is dropped from the session.
        office
            .core
            .start_session(
                &fabric,
                &office.id(),
                group,
                GroupAudioSource::PickerToken(1),
                [2; 16],
                now,
            )
            .unwrap();
        let out = office
            .core
            .pump(&fabric, &office.id(), true, now + ADMISSION_TIMEOUT);
        assert!(out.contains(&stop_to(&shelf.id(), shelf_route([2; 16]), None)));
        let plan = plan_of(&office, [2; 16]);
        assert_eq!(plan.members.len(), 1);
    }

    phase a_source_going_offline_ends_the_members_session_and_not_the_consent;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (mut fabric, group) = joined_pair(&mut office, &mut shelf, now);
        let session = [8; 16];
        admitted_session(&mut office, &mut shelf, &fabric, group, session, now);
        office.online = false;
        office.publish(&mut fabric);
        shelf.core.pump(&fabric, &shelf.id(), true, now);
        assert!(!shelf.core.inbound.contains_key(&shelf_route(session)));
        // Consent itself survives a leader going offline.
        assert!(shelf.core.memberships.iter().flatten().next().is_some());
    }

    phase only_certified_devices_that_can_join_are_admitted;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let mut fabric = PersonDevices::default();
        office.publish(&mut fabric);
        let stranger = office.core.create_group(
            &fabric,
            &office.id(),
            SpeakerGroupId([1; 16]),
            "Living room",
            &pair_members(),
            now,
        );
        assert!(stranger.is_err(), "shelf is not in the fabric");
        shelf.audio_group = false;
        shelf.publish(&mut fabric);
        assert!(
            office.core
                .create_group(
                    &fabric,
                    &office.id(),
                    SpeakerGroupId([1; 16]),
                    "Living room",
                    &pair_members(),
                    now,
                )
                .is_err(),
            "shelf has media continuity off"
        );
        let mut missing = pair_members();
        missing[1].output = EndpointSelector::mint(&id("shelf"), "unplugged");
        shelf.audio_group = true;
        shelf.publish(&mut fabric);
        assert!(
            office.core
                .create_group(
                    &fabric,
                    &office.id(),
                    SpeakerGroupId([1; 16]),
                    "Living room",
                    &missing,
                    now
                )
                .is_err(),
            "shelf does not offer that output"
        );
    }
}

tilo_test_env::scenario! {
    fn consent_ends_when_the_group_changes_under_a_member;

    phase leaving_withdraws_consent_ends_sessions_and_silences_invites;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (mut fabric, group) = joined_pair(&mut office, &mut shelf, now);
        let session = [8; 16];
        let route = admitted_session(&mut office, &mut shelf, &fabric, group, session, now);
        let out = shelf.core.leave(group).unwrap();
        assert!(out.contains(&leave_to(&office.id(), group)));
        assert!(out.contains(&stop_to(&office.id(), route, Some([9; 16]))));
        assert!(
            shelf
                .core
                .grant(&fabric, &shelf.id(), route, &office.id())
                .is_none()
        );
        shelf.publish(&mut fabric);
        for item in &out {
            office.handle(&fabric, item, "shelf", now);
        }
        let plan = plan_of(&office, session);
        assert_eq!(plan.members.len(), 1);
        assert!(
            office
                .core
                .pump(&fabric, &office.id(), true, now + INVITE_RETRY * 2)
                .is_empty(),
            "a member that left is not nagged"
        );
    }

    phase changing_a_members_role_voids_its_consent_and_asks_again;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (mut fabric, group) = joined_pair(&mut office, &mut shelf, now);
        let session = [8; 16];
        admitted_session(&mut office, &mut shelf, &fabric, group, session, now);
        let mut swapped = pair_members();
        swapped[0].role = RIGHT;
        swapped[1].role = LEFT;
        let out = office
            .core
            .update_group(&fabric, &office.id(), group, "Living room", &swapped, now)
            .unwrap();
        assert!(out.iter().any(|item| matches!(
            item.action,
            PersonalFabricAction::SpeakerGroupInvite { role: LEFT, .. }
        )));
        office.publish(&mut fabric);
        assert_eq!(
            fabric.speaker_member_states(fabric.speaker_group(group).unwrap()),
            vec![
                SpeakerMemberStatus::Ready,
                SpeakerMemberStatus::AwaitingConsent
            ]
        );
        shelf.core.pump(&fabric, &shelf.id(), true, now);
        assert!(!shelf.core.inbound.contains_key(&shelf_route(session)));
        assert!(shelf.core.memberships.iter().flatten().next().is_none());
    }
}

tilo_test_env::scenario! {
    fn a_groups_teardown_releases_its_members;

    phase deleting_a_group_releases_members_and_they_drop_their_consent;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (mut fabric, group) = joined_pair(&mut office, &mut shelf, now);
        let session = [8; 16];
        let route = admitted_session(&mut office, &mut shelf, &fabric, group, session, now);
        let out = office.core.delete_group(group).unwrap();
        assert!(out.contains(&leave_to(&shelf.id(), group)));
        assert!(office.core.playback_plans().is_empty());
        office.publish(&mut fabric);
        // Even if the leave message is lost, the member notices the group is
        // gone from its leader's part of the person's store.
        let teardown = shelf.core.pump(&fabric, &shelf.id(), true, now);
        assert!(teardown.contains(&stop_to(&office.id(), route, Some([9; 16]))));
        assert!(shelf.core.memberships.iter().flatten().next().is_none());
    }

    phase disabling_media_continuity_tears_everything_down;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (fabric, group) = joined_pair(&mut office, &mut shelf, now);
        let session = [8; 16];
        let route = admitted_session(&mut office, &mut shelf, &fabric, group, session, now);
        let out = shelf.core.pump(&fabric, &shelf.id(), false, now);
        assert!(out.contains(&stop_to(&office.id(), route, Some([9; 16]))));
        assert!(out.contains(&leave_to(&office.id(), group)));
        assert!(shelf.core.memberships.iter().flatten().next().is_none());
        let leader = office.core.pump(&fabric, &office.id(), false, now);
        assert!(leader.contains(&stop_to(&shelf.id(), route, Some([9; 16]))));
        assert!(office.core.playback_plans().is_empty());
    }
}

tilo_test_env::scenario! {
    fn admitted_sessions_become_routes_by_endpoint_selector;

    phase a_surround_set_resolves_member_channels_for_grants;
    {
        let now = Instant::now();
        let (mut office, mut shelf, mut sub) = (
            Device::new("office"),
            Device::new("shelf"),
            Device::new("sub"),
        );
        let mut fabric = PersonDevices::default();
        office.publish(&mut fabric);
        shelf.publish(&mut fabric);
        sub.publish(&mut fabric);
        let mut members = pair_members();
        members.push(MemberSpec {
            device: id("sub"),
            output: output("sub"),
            role: SpeakerRole::Surround(SurroundChannel::Lfe),
        });
        let group = SpeakerGroupId([2; 16]);
        let invites = office
            .core
            .create_group(&fabric, &office.id(), group, "Cinema", &members, now)
            .unwrap();
        office.publish(&mut fabric);
        let to_sub = invites.iter().find(|item| item.target == sub.id()).unwrap();
        sub.handle(&fabric, to_sub, "office", now);
        sub.core
            .resolve_consent(&fabric, &sub.id(), group, true, &stamp("sub"))
            .unwrap();
        sub.publish(&mut fabric);
        let offer = egress(
            &sub.id(),
            PersonalFabricAction::SpeakerGroupSessionOffer {
                session_id: [4; 16],
                group,
            },
        );
        sub.handle(&fabric, &offer, "office", now);
        let grant = sub
            .core
            .grant(&fabric, &sub.id(), [4; 16], &office.id())
            .unwrap();
        assert_eq!(grant.channel, ChannelRole::Lfe);
    }

    phase endpoints_are_published_by_selector_without_node_identities;
    {
        let graph = media::audio::AudioGraph {
            sinks: vec![media::audio::AudioDevice {
                id: 57,
                serial: 1204,
                name: "alsa_output.pci-0000_66_00.6.analog-stereo".into(),
                description: "Speakers".into(),
                is_default: true,
                volume: None,
                muted: None,
                channels: 2,
                codec: None,
            }],
            sources: vec![media::audio::AudioDevice {
                id: 58,
                serial: 1205,
                name: "alsa_input.usb-mic".into(),
                description: String::new(),
                is_default: false,
                volume: None,
                muted: None,
                channels: 1,
                codec: None,
            }],
            ..media::audio::AudioGraph::default()
        };
        let endpoints = endpoints_from_graph(&id("office"), &graph);
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].kind, AudioEndpointKind::Output);
        assert_eq!(endpoints[0].label, "Speakers");
        assert_eq!(endpoints[1].kind, AudioEndpointKind::Microphone);
        assert_eq!(endpoints[1].label, "alsa_input.usb-mic");
        let wire = postcard::to_allocvec(&endpoints[0]).unwrap();
        assert!(!wire.windows(4).any(|window| window == b"alsa"));
    }

    phase admitted_sessions_become_send_local_and_receive_routes;
    {
        let now = Instant::now();
        let (mut office, mut shelf) = (Device::new("office"), Device::new("shelf"));
        let (fabric, group) = joined_pair(&mut office, &mut shelf, now);
        let session = [8; 16];
        let route = admitted_session(&mut office, &mut shelf, &fabric, group, session, now);
        let leader = office.core.route_specs(&fabric, &office.id());
        assert_eq!(leader.len(), 2);
        assert!(leader.iter().any(|spec| matches!(
            spec,
            RouteSpec::Local { channel: ChannelRole::Left, output: out, .. } if *out == output("office")
        )));
        assert!(leader.contains(&RouteSpec::Send {
            route,
            target: shelf.id(),
            authorization: [9; 16],
            source: GroupAudioSource::PickerToken(1),
        }));
        let member = shelf.core.route_specs(&fabric, &shelf.id());
        assert_eq!(
            member,
            vec![RouteSpec::Receive {
                route,
                source: office.id(),
                authorization: [9; 16],
                output: output("shelf"),
                channel: ChannelRole::Right,
                trim: SpeakerTrim::default(),
            }]
        );
        assert!(shelf.core.admits(route, &office.id()));
        assert!(!shelf.core.admits(route, &id("phone")));
    }

    phase route_changes_start_new_end_gone_and_restart_changed_routes;
    {
        let spec = |route: u8, delay_ns| RouteSpec::Receive {
            route: [route; 16],
            source: id("office"),
            authorization: [9; 16],
            output: output("shelf"),
            channel: ChannelRole::Right,
            trim: SpeakerTrim {
                delay_ns,
                ..SpeakerTrim::default()
            },
        };
        let running = BTreeMap::from([([1; 16], spec(1, 0)), ([2; 16], spec(2, 0))]);
        let (end, start) = route_changes(&running, &[spec(2, 5_000), spec(3, 0)]);
        assert_eq!(end, vec![[1; 16], [2; 16]]);
        assert_eq!(start, vec![spec(2, 5_000), spec(3, 0)]);
        let (end, start) = route_changes(&running, &[spec(1, 0), spec(2, 0)]);
        assert!(end.is_empty() && start.is_empty());
    }
}

tilo_test_env::scenario! {
    fn a_groups_microphones_listen_only_for_the_leader_and_with_consent;

    phase a_microphone_listens_only_after_its_own_device_consents;
    {
        let now = Instant::now();
        let (mut office, mut shelf, mut phone) = (
            Device::new("office"),
            Device::new("shelf"),
            Device::new("phone"),
        );
        let (mut fabric, group) = joined_pair(&mut office, &mut shelf, now);
        phone.publish(&mut fabric);

        // Only the leader may ask a consented member for a sweep.
        assert_eq!(
            shelf
                .core
                .sweep_output(&fabric, &shelf.id(), &office.id(), group),
            Some(output("shelf"))
        );
        assert_eq!(
            shelf
                .core
                .sweep_output(&fabric, &shelf.id(), &id("phone"), group),
            None
        );

        let published = fabric.speaker_group(group).unwrap().clone();
        let candidates = office
            .core
            .listener_candidates(&fabric, &office.id(), &published, now);
        assert_eq!(candidates[0].device, office.id(), "this device comes first");
        assert!(candidates[0].local);
        assert_eq!(candidates[0].state, ListenerState::Ready);
        assert_eq!(
            candidate_state(&office, &fabric, group, "phone", now),
            ListenerState::NotAsked
        );
        // Playing in the group is not consent to listen.
        assert_eq!(
            candidate_state(&office, &fabric, group, "shelf", now),
            ListenerState::NotAsked
        );

        let invites =
            office
                .core
                .invite_listeners(&published, &[(phone.id(), microphone("phone"))], now);
        assert_eq!(
            invites,
            vec![Egress {
                target: phone.id(),
                action: PersonalFabricAction::SpeakerListenInvite {
                    group,
                    name: "Living room".into(),
                    microphone: microphone("phone"),
                },
            }]
        );
        assert!(
            office
                .core
                .invite_listeners(&published, &[(phone.id(), microphone("phone"))], now)
                .is_empty(),
            "asked once"
        );
        assert_eq!(
            candidate_state(&office, &fabric, group, "phone", now),
            ListenerState::AwaitingConsent
        );

        assert!(phone.handle(&fabric, &invites[0], "office", now).is_empty());
        assert!(!phone.core.may_listen(
            &fabric,
            &phone.id(),
            &office.id(),
            group,
            microphone("phone")
        ));
        let view = phone.core.view(&fabric, &phone.id(), &|_| None);
        assert_eq!(view.consents.len(), 1);
        assert_eq!(view.consents[0].scope, SpeakerConsentScope::Listen);
        assert_eq!(
            view.consents[0].endpoint_label.as_deref(),
            Some("phone microphone")
        );
        assert_eq!(
            view.consents[0].prompt(),
            "Let office use this microphone for Living room?"
        );

        let decision = phone
            .core
            .resolve_listen_consent(&fabric, &phone.id(), group, true, &stamp("phone"))
            .unwrap();
        phone.publish(&mut fabric);
        assert!(
            office
                .handle(&fabric, &decision[0], "phone", now)
                .is_empty()
        );
        assert_eq!(
            candidate_state(&office, &fabric, group, "phone", now),
            ListenerState::Ready
        );
        assert!(phone.core.may_listen(
            &fabric,
            &phone.id(),
            &office.id(),
            group,
            microphone("phone")
        ));
        // Only the leader may ask, and only for the exact microphone.
        assert!(!phone.core.may_listen(
            &fabric,
            &phone.id(),
            &shelf.id(),
            group,
            microphone("phone")
        ));
        assert!(
            !phone
                .core
                .may_listen(&fabric, &phone.id(), &office.id(), group, output("phone"))
        );
        let memberships = phone.core.view(&fabric, &phone.id(), &|_| None).memberships;
        assert_eq!(memberships.len(), 1);
        assert_eq!(memberships[0].scope, SpeakerConsentScope::Listen);

        // Withdrawing tells the leader, stops listening at once, and ends the
        // leader's sweep.
        let withdrawn = phone.core.stop_listening(group).unwrap();
        assert_eq!(
            withdrawn[0].action,
            PersonalFabricAction::SpeakerListenDecision {
                group,
                accepted: false
            }
        );
        assert!(!phone.core.may_listen(
            &fabric,
            &phone.id(),
            &office.id(),
            group,
            microphone("phone")
        ));
        assert_eq!(
            phone
                .core
                .sweep_output(&fabric, &phone.id(), &office.id(), group),
            None
        );
    }

    phase a_declined_or_ended_listening_consent_is_not_asked_again_or_kept;
    {
        let now = Instant::now();
        let (mut office, mut shelf, mut phone) = (
            Device::new("office"),
            Device::new("shelf"),
            Device::new("phone"),
        );
        let (mut fabric, group) = joined_pair(&mut office, &mut shelf, now);
        phone.publish(&mut fabric);
        let published = fabric.speaker_group(group).unwrap().clone();
        let asked = [(phone.id(), microphone("phone"))];

        // A declined microphone is not asked again until the prompt expires.
        let invites = office.core.invite_listeners(&published, &asked, now);
        phone.handle(&fabric, &invites[0], "office", now);
        let decision = phone
            .core
            .resolve_listen_consent(&fabric, &phone.id(), group, false, &stamp("phone"))
            .unwrap();
        office.handle(&fabric, &decision[0], "phone", now);
        assert_eq!(
            candidate_state(&office, &fabric, group, "phone", now),
            ListenerState::Declined
        );
        assert!(
            office
                .core
                .invite_listeners(&published, &asked, now)
                .is_empty()
        );
        let later = now + CONSENT_TTL;
        assert_eq!(
            candidate_state(&office, &fabric, group, "phone", later),
            ListenerState::NotAsked
        );

        // Once the prompt expired the leader asks again, and the answer is
        // consent.
        let consent = |phone: &mut Device, office: &mut Device, fabric: &mut PersonDevices, at| {
            let invites = office.core.invite_listeners(&published, &asked, at);
            assert_eq!(invites.len(), 1);
            for invite in &invites {
                phone.handle(fabric, invite, "office", at);
            }
            phone
                .core
                .resolve_listen_consent(fabric, &phone.id(), group, true, &stamp("phone"))
                .unwrap();
            phone.publish(fabric);
        };
        consent(&mut phone, &mut office, &mut fabric, later);
        let stopped = phone.core.pump(&fabric, &phone.id(), false, now);
        assert!(stopped.iter().any(|item| {
            item.target == office.id()
                && item.action
                    == PersonalFabricAction::SpeakerListenDecision {
                        group,
                        accepted: false,
                    }
        }));
        assert!(phone.core.listeners.iter().flatten().next().is_none());

        // And with the group: deleting it releases the listening device.
        consent(
            &mut phone,
            &mut office,
            &mut fabric,
            later + CONSENT_TTL,
        );
        let released = office.core.delete_group(group).unwrap();
        office.publish(&mut fabric);
        let leave = released
            .iter()
            .find(|item| item.target == phone.id())
            .expect("the listening device is released too");
        phone.handle(&fabric, leave, "office", now);
        assert!(phone.core.listeners.iter().flatten().next().is_none());
    }
}

fn candidate_state(
    office: &Device,
    fabric: &PersonDevices,
    group: SpeakerGroupId,
    device: &str,
    now: Instant,
) -> ListenerState {
    let published = fabric.speaker_group(group).unwrap();
    office
        .core
        .listener_candidates(fabric, &office.id(), published, now)
        .into_iter()
        .find(|candidate| candidate.device == id(device))
        .unwrap()
        .state
}
