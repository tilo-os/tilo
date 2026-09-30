use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use media::sync_playout::ChannelRole;

use super::*;
use crate::shell::device_fabric::{
    DeviceFabricSnapshot, DeviceKind, DevicePresence, FABRIC_SCHEMA_VERSION, FabricCapabilities,
};
use crate::shell::speaker_group::{StereoSide, SurroundChannel};

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

    /// Publish this device's current records into `fabric`.
    fn publish(&mut self, fabric: &mut DeviceFabric) {
        self.counter = self.counter.saturating_add(1);
        let owner = self.id();
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
        let snapshot = DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: owner.clone(),
            presence: DevicePresence {
                device: owner.clone(),
                label: self.name.into(),
                kind: DeviceKind::Speaker,
                capabilities: FabricCapabilities {
                    audio_output: true,
                    audio_group: self.audio_group,
                    ..FabricCapabilities::default()
                },
                online: self.online,
                last_seen_unix_ms: self.counter,
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
            speakers: self.core.authored(endpoints, &stamp(self.name)),
        };
        assert_eq!(fabric.upsert(snapshot), Ok(true));
    }

    fn handle(
        &mut self,
        fabric: &DeviceFabric,
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
            device: id("desk"),
            output: output("desk"),
            role: LEFT,
        },
        MemberSpec {
            device: id("shelf"),
            output: output("shelf"),
            role: RIGHT,
        },
    ]
}

/// Desk leads a stereo pair with shelf; returns the fabric with both
/// published and the group id, before shelf has answered.
fn invited_pair(
    desk: &mut Device,
    shelf: &mut Device,
    now: Instant,
) -> (DeviceFabric, SpeakerGroupId, Vec<Egress>) {
    let mut fabric = DeviceFabric::default();
    desk.publish(&mut fabric);
    shelf.publish(&mut fabric);
    let group = SpeakerGroupId([4; 16]);
    let invites = desk
        .core
        .create_group(
            &fabric,
            &desk.id(),
            group,
            "Living room",
            &pair_members(),
            now,
        )
        .unwrap();
    desk.publish(&mut fabric);
    (fabric, group, invites)
}

/// Desk and shelf form a consented stereo pair.
fn joined_pair(
    desk: &mut Device,
    shelf: &mut Device,
    now: Instant,
) -> (DeviceFabric, SpeakerGroupId) {
    let (mut fabric, group, invites) = invited_pair(desk, shelf, now);
    assert_eq!(invites.len(), 1);
    assert!(shelf.handle(&fabric, &invites[0], "desk", now).is_empty());
    let decision = shelf
        .core
        .resolve_consent(&fabric, &shelf.id(), group, true, &stamp("shelf"))
        .unwrap();
    shelf.publish(&mut fabric);
    assert!(desk.handle(&fabric, &decision[0], "shelf", now).is_empty());
    (fabric, group)
}

#[test]
fn a_new_group_asks_only_remote_members_and_the_member_sees_one_prompt() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group, invites) = invited_pair(&mut desk, &mut shelf, now);
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
            SpeakerMemberState::Ready,
            SpeakerMemberState::AwaitingConsent
        ]
    );
    shelf.handle(&fabric, &invites[0], "desk", now);
    let view = shelf.core.view(&fabric, &shelf.id(), &|_| None);
    assert_eq!(view.consents.len(), 1);
    let consent = &view.consents[0];
    assert_eq!(consent.name, "Living room");
    assert_eq!(consent.leader_label, "desk");
    assert_eq!(consent.scope, SpeakerConsentScope::Play(RIGHT));
    assert_eq!(consent.endpoint_label.as_deref(), Some("shelf speakers"));
    assert_eq!(consent.prompt(), "Join Living room as the right speaker?");
}

#[test]
fn approving_authors_a_membership_that_makes_the_member_ready() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    assert_eq!(
        fabric.speaker_member_states(fabric.speaker_group(group).unwrap()),
        vec![SpeakerMemberState::Ready, SpeakerMemberState::Ready]
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
        desk.core
            .pump(&fabric, &desk.id(), true, now + INVITE_RETRY)
            .is_empty()
    );
}

#[test]
fn a_forged_invite_that_the_leader_never_published_cannot_be_approved() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group, _) = invited_pair(&mut desk, &mut shelf, now);
    let forged = egress(
        &id("shelf"),
        PersonalFabricAction::SpeakerGroupInvite {
            group,
            name: "Living room".into(),
            output: output("shelf"),
            role: LEFT,
        },
    );
    shelf.handle(&fabric, &forged, "desk", now);
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
}

#[test]
fn declining_stops_the_leader_asking_until_the_group_is_edited() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group, invites) = invited_pair(&mut desk, &mut shelf, now);
    shelf.handle(&fabric, &invites[0], "desk", now);
    let decision = shelf
        .core
        .resolve_consent(&fabric, &shelf.id(), group, false, &stamp("shelf"))
        .unwrap();
    desk.handle(&fabric, &decision[0], "shelf", now);
    assert!(
        desk.core
            .pump(&fabric, &desk.id(), true, now + INVITE_RETRY * 3)
            .is_empty()
    );
    let edited = desk
        .core
        .update_group(&fabric, &desk.id(), group, "Lounge", &pair_members(), now)
        .unwrap();
    assert!(
        edited
            .iter()
            .any(|item| matches!(item.action, PersonalFabricAction::SpeakerGroupInvite { .. }))
    );
}

#[test]
fn an_unanswered_invite_is_repeated_while_the_member_is_online() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, _, _) = invited_pair(&mut desk, &mut shelf, now);
    assert!(desk.core.pump(&fabric, &desk.id(), true, now).is_empty());
    assert_eq!(
        desk.core
            .pump(&fabric, &desk.id(), true, now + INVITE_RETRY)
            .len(),
        1
    );
}

#[test]
fn any_of_your_devices_can_play_to_a_consented_group() {
    let now = Instant::now();
    let (mut desk, mut shelf, mut phone) = (
        Device::new("desk"),
        Device::new("shelf"),
        Device::new("phone"),
    );
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    phone.publish(&mut fabric);
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
        let (member, from) = if offer.target == desk.id() {
            (&mut desk, "desk")
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
            ("desk".into(), ChannelRole::Left, Some([9; 16])),
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
            .grant(&fabric, &shelf.id(), shelf_route(session), &desk.id())
            .is_none()
    );
}

#[test]
fn the_leader_plays_its_own_output_locally_without_asking_itself() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let offers = desk
        .core
        .start_session(
            &fabric,
            &desk.id(),
            group,
            GroupAudioSource::MediaSession("movie".into()),
            [5; 16],
            now,
        )
        .unwrap();
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].target, shelf.id());
    let plan = &desk.core.playback_plans()[0];
    assert_eq!(plan.members.len(), 1, "shelf has not admitted yet");
    assert_eq!(plan.members[0].device, desk.id());
    assert_eq!(plan.members[0].authorization, None);
}

#[test]
fn a_member_without_consent_refuses_a_session() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group, _) = invited_pair(&mut desk, &mut shelf, now);
    let offer = egress(
        &shelf.id(),
        PersonalFabricAction::SpeakerGroupSessionOffer {
            session_id: [6; 16],
            group,
        },
    );
    let reply = shelf.handle(&fabric, &offer, "desk", now);
    assert_eq!(
        reply,
        vec![egress(
            &desk.id(),
            PersonalFabricAction::SpeakerGroupSessionDecision {
                session_id: [6; 16],
                group,
                authorization: None,
            },
        )]
    );
    assert!(shelf.core.inbound.is_empty());
}

fn admitted_session(
    desk: &mut Device,
    shelf: &mut Device,
    fabric: &DeviceFabric,
    group: SpeakerGroupId,
    now: Instant,
) -> [u8; 16] {
    let session = [8; 16];
    let offers = desk
        .core
        .start_session(
            fabric,
            &desk.id(),
            group,
            GroupAudioSource::PickerToken(1),
            session,
            now,
        )
        .unwrap();
    let decision = shelf.handle(fabric, &offers[0], "desk", now);
    desk.handle(fabric, &decision[0], "shelf", now);
    assert_eq!(desk.core.playback_plans()[0].members.len(), 2);
    shelf_route(session)
}

#[test]
fn leaving_withdraws_consent_ends_sessions_and_silences_invites() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let session = admitted_session(&mut desk, &mut shelf, &fabric, group, now);
    let out = shelf.core.leave(group).unwrap();
    assert!(out.contains(&egress(
        &desk.id(),
        PersonalFabricAction::SpeakerGroupLeave { group }
    )));
    assert!(out.contains(&egress(
        &desk.id(),
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: session,
            authorization: Some([9; 16]),
        }
    )));
    assert!(
        shelf
            .core
            .grant(&fabric, &shelf.id(), session, &desk.id())
            .is_none()
    );
    shelf.publish(&mut fabric);
    for item in &out {
        desk.handle(&fabric, item, "shelf", now);
    }
    assert_eq!(desk.core.playback_plans()[0].members.len(), 1);
    assert!(
        desk.core
            .pump(&fabric, &desk.id(), true, now + INVITE_RETRY * 2)
            .is_empty(),
        "a member that left is not nagged"
    );
}

#[test]
fn deleting_a_group_releases_members_and_they_drop_their_consent() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let session = admitted_session(&mut desk, &mut shelf, &fabric, group, now);
    let out = desk.core.delete_group(group).unwrap();
    assert!(out.contains(&egress(
        &shelf.id(),
        PersonalFabricAction::SpeakerGroupLeave { group }
    )));
    assert!(desk.core.playback_plans().is_empty());
    desk.publish(&mut fabric);
    // Even if the leave message is lost, the member notices the group is
    // gone from its leader's snapshot.
    let teardown = shelf.core.pump(&fabric, &shelf.id(), true, now);
    assert!(teardown.contains(&egress(
        &desk.id(),
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: session,
            authorization: Some([9; 16]),
        }
    )));
    assert!(shelf.core.memberships.iter().flatten().next().is_none());
}

#[test]
fn changing_a_members_role_voids_its_consent_and_asks_again() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let session = admitted_session(&mut desk, &mut shelf, &fabric, group, now);
    let mut swapped = pair_members();
    swapped[0].role = RIGHT;
    swapped[1].role = LEFT;
    let out = desk
        .core
        .update_group(&fabric, &desk.id(), group, "Living room", &swapped, now)
        .unwrap();
    assert!(out.iter().any(|item| matches!(
        item.action,
        PersonalFabricAction::SpeakerGroupInvite { role: LEFT, .. }
    )));
    desk.publish(&mut fabric);
    assert_eq!(
        fabric.speaker_member_states(fabric.speaker_group(group).unwrap()),
        vec![
            SpeakerMemberState::Ready,
            SpeakerMemberState::AwaitingConsent
        ]
    );
    shelf.core.pump(&fabric, &shelf.id(), true, now);
    assert!(!shelf.core.inbound.contains_key(&session));
    assert!(shelf.core.memberships.iter().flatten().next().is_none());
}

#[test]
fn a_calibration_trim_keeps_consent() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let trim = SpeakerTrim {
        delay_ns: 2_500_000,
        gain_mb: -150,
        eq: Vec::new(),
    };
    desk.core
        .set_trims(group, &[(shelf.id(), output("shelf"), trim.clone())])
        .unwrap();
    desk.publish(&mut fabric);
    let published = fabric.speaker_group(group).unwrap();
    assert_eq!(published.member(&shelf.id()).unwrap().trim, trim);
    assert_eq!(
        fabric.speaker_member_states(published),
        vec![SpeakerMemberState::Ready, SpeakerMemberState::Ready]
    );
    assert!(
        desk.core
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

#[test]
fn disabling_media_continuity_tears_everything_down() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let session = admitted_session(&mut desk, &mut shelf, &fabric, group, now);
    let out = shelf.core.pump(&fabric, &shelf.id(), false, now);
    assert!(out.contains(&egress(
        &desk.id(),
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: session,
            authorization: Some([9; 16]),
        }
    )));
    assert!(out.contains(&egress(
        &desk.id(),
        PersonalFabricAction::SpeakerGroupLeave { group }
    )));
    assert!(shelf.core.memberships.iter().flatten().next().is_none());
    let leader = desk.core.pump(&fabric, &desk.id(), false, now);
    assert!(leader.contains(&egress(
        &shelf.id(),
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: session,
            authorization: Some([9; 16]),
        }
    )));
    assert!(desk.core.playback_plans().is_empty());
}

#[test]
fn a_source_going_offline_ends_the_members_session() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let session = admitted_session(&mut desk, &mut shelf, &fabric, group, now);
    desk.online = false;
    desk.publish(&mut fabric);
    shelf.core.pump(&fabric, &shelf.id(), true, now);
    assert!(!shelf.core.inbound.contains_key(&session));
    // Consent itself survives a leader going offline.
    assert!(shelf.core.memberships.iter().flatten().next().is_some());
}

#[test]
fn a_member_that_never_answers_is_dropped_from_the_session() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    desk.core
        .start_session(
            &fabric,
            &desk.id(),
            group,
            GroupAudioSource::PickerToken(1),
            [2; 16],
            now,
        )
        .unwrap();
    let out = desk
        .core
        .pump(&fabric, &desk.id(), true, now + ADMISSION_TIMEOUT);
    assert!(out.contains(&egress(
        &shelf.id(),
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: shelf_route([2; 16]),
            authorization: None,
        }
    )));
    assert_eq!(desk.core.playback_plans()[0].members.len(), 1);
}

#[test]
fn a_late_admission_for_an_ended_session_is_revoked() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let late = egress(
        &desk.id(),
        PersonalFabricAction::SpeakerGroupSessionDecision {
            session_id: [1; 16],
            group,
            authorization: Some([7; 16]),
        },
    );
    assert_eq!(
        desk.handle(&fabric, &late, "shelf", now),
        vec![egress(
            &shelf.id(),
            PersonalFabricAction::SpeakerGroupSessionStop {
                session_id: [1; 16],
                authorization: Some([7; 16]),
            }
        )]
    );
}

#[test]
fn a_session_stop_from_anyone_but_the_source_is_ignored() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let session = admitted_session(&mut desk, &mut shelf, &fabric, group, now);
    let stop = egress(
        &shelf.id(),
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: session,
            authorization: Some([9; 16]),
        },
    );
    shelf.handle(&fabric, &stop, "phone", now);
    assert!(shelf.core.inbound.contains_key(&session));
    shelf.handle(&fabric, &stop, "desk", now);
    assert!(!shelf.core.inbound.contains_key(&session));
}

#[test]
fn only_certified_devices_that_can_join_are_admitted() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let mut fabric = DeviceFabric::default();
    desk.publish(&mut fabric);
    let stranger = desk.core.create_group(
        &fabric,
        &desk.id(),
        SpeakerGroupId([1; 16]),
        "Living room",
        &pair_members(),
        now,
    );
    assert!(stranger.is_err(), "shelf is not in the fabric");
    shelf.audio_group = false;
    shelf.publish(&mut fabric);
    assert!(
        desk.core
            .create_group(
                &fabric,
                &desk.id(),
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
        desk.core
            .create_group(
                &fabric,
                &desk.id(),
                SpeakerGroupId([1; 16]),
                "Living room",
                &missing,
                now
            )
            .is_err(),
        "shelf does not offer that output"
    );
}

#[test]
fn a_stale_prompt_expires() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group, invites) = invited_pair(&mut desk, &mut shelf, now);
    shelf.handle(&fabric, &invites[0], "desk", now);
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

#[test]
fn a_surround_set_resolves_member_channels_for_grants() {
    let now = Instant::now();
    let (mut desk, mut shelf, mut sub) = (
        Device::new("desk"),
        Device::new("shelf"),
        Device::new("sub"),
    );
    let mut fabric = DeviceFabric::default();
    desk.publish(&mut fabric);
    shelf.publish(&mut fabric);
    sub.publish(&mut fabric);
    let mut members = pair_members();
    members.push(MemberSpec {
        device: id("sub"),
        output: output("sub"),
        role: SpeakerRole::Surround(SurroundChannel::Lfe),
    });
    let group = SpeakerGroupId([2; 16]);
    let invites = desk
        .core
        .create_group(&fabric, &desk.id(), group, "Cinema", &members, now)
        .unwrap();
    desk.publish(&mut fabric);
    let to_sub = invites.iter().find(|item| item.target == sub.id()).unwrap();
    sub.handle(&fabric, to_sub, "desk", now);
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
    sub.handle(&fabric, &offer, "desk", now);
    let grant = sub
        .core
        .grant(&fabric, &sub.id(), [4; 16], &desk.id())
        .unwrap();
    assert_eq!(grant.channel, ChannelRole::Lfe);
}

#[test]
fn endpoints_are_published_by_selector_without_node_identities() {
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
    let endpoints = endpoints_from_graph(&id("desk"), &graph);
    assert_eq!(endpoints.len(), 2);
    assert_eq!(endpoints[0].kind, AudioEndpointKind::Output);
    assert_eq!(endpoints[0].label, "Speakers");
    assert_eq!(endpoints[1].kind, AudioEndpointKind::Microphone);
    assert_eq!(endpoints[1].label, "alsa_input.usb-mic");
    let wire = postcard::to_allocvec(&endpoints[0]).unwrap();
    assert!(!wire.windows(4).any(|window| window == b"alsa"));
}

#[test]
fn admitted_sessions_become_send_local_and_receive_routes() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let route = admitted_session(&mut desk, &mut shelf, &fabric, group, now);
    let leader = desk.core.route_specs(&fabric, &desk.id());
    assert_eq!(leader.len(), 2);
    assert!(leader.iter().any(|spec| matches!(
        spec,
        RouteSpec::Local { channel: ChannelRole::Left, output: out, .. } if *out == output("desk")
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
            source: desk.id(),
            authorization: [9; 16],
            output: output("shelf"),
            channel: ChannelRole::Right,
            trim: SpeakerTrim::default(),
        }]
    );
    assert!(shelf.core.admits(route, &desk.id()));
    assert!(!shelf.core.admits(route, &id("phone")));
}

#[test]
fn route_changes_start_new_end_gone_and_restart_changed_routes() {
    let spec = |route: u8, delay_ns| RouteSpec::Receive {
        route: [route; 16],
        source: id("desk"),
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

#[test]
fn a_route_the_audio_path_lost_ends_its_member_session() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    let route = admitted_session(&mut desk, &mut shelf, &fabric, group, now);
    assert_eq!(
        shelf.core.route_lost(route),
        vec![egress(
            &desk.id(),
            PersonalFabricAction::SpeakerGroupSessionStop {
                session_id: route,
                authorization: Some([9; 16]),
            }
        )]
    );
    assert_eq!(
        desk.core.route_lost(route),
        vec![egress(
            &shelf.id(),
            PersonalFabricAction::SpeakerGroupSessionStop {
                session_id: route,
                authorization: Some([9; 16]),
            }
        )]
    );
    assert_eq!(desk.core.playback_plans()[0].members.len(), 1);
}

#[test]
fn only_the_leader_can_ask_a_consented_member_for_a_sweep() {
    let now = Instant::now();
    let (mut desk, mut shelf) = (Device::new("desk"), Device::new("shelf"));
    let (fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    assert_eq!(
        shelf
            .core
            .sweep_output(&fabric, &shelf.id(), &desk.id(), group),
        Some(output("shelf"))
    );
    assert_eq!(
        shelf
            .core
            .sweep_output(&fabric, &shelf.id(), &id("phone"), group),
        None
    );
    shelf.core.leave(group).unwrap();
    assert_eq!(
        shelf
            .core
            .sweep_output(&fabric, &shelf.id(), &desk.id(), group),
        None
    );
}

fn candidate_state(
    desk: &Device,
    fabric: &DeviceFabric,
    group: SpeakerGroupId,
    device: &str,
    now: Instant,
) -> ListenerState {
    let published = fabric.speaker_group(group).unwrap();
    desk.core
        .listener_candidates(fabric, &desk.id(), published, now)
        .into_iter()
        .find(|candidate| candidate.device == id(device))
        .unwrap()
        .state
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one consent exchange, followed from invite to withdrawal"
)]
fn a_microphone_listens_only_after_its_own_device_consents() {
    let now = Instant::now();
    let (mut desk, mut shelf, mut phone) = (
        Device::new("desk"),
        Device::new("shelf"),
        Device::new("phone"),
    );
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    phone.publish(&mut fabric);
    let published = fabric.speaker_group(group).unwrap().clone();
    let candidates = desk
        .core
        .listener_candidates(&fabric, &desk.id(), &published, now);
    assert_eq!(candidates[0].device, desk.id(), "this device comes first");
    assert!(candidates[0].local);
    assert_eq!(candidates[0].state, ListenerState::Ready);
    assert_eq!(
        candidate_state(&desk, &fabric, group, "phone", now),
        ListenerState::NotAsked
    );
    // Playing in the group is not consent to listen.
    assert_eq!(
        candidate_state(&desk, &fabric, group, "shelf", now),
        ListenerState::NotAsked
    );

    let invites = desk
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
        desk.core
            .invite_listeners(&published, &[(phone.id(), microphone("phone"))], now)
            .is_empty(),
        "asked once"
    );
    assert_eq!(
        candidate_state(&desk, &fabric, group, "phone", now),
        ListenerState::AwaitingConsent
    );

    assert!(phone.handle(&fabric, &invites[0], "desk", now).is_empty());
    assert!(
        !phone
            .core
            .may_listen(&fabric, &phone.id(), &desk.id(), group, microphone("phone"))
    );
    let view = phone.core.view(&fabric, &phone.id(), &|_| None);
    assert_eq!(view.consents.len(), 1);
    assert_eq!(view.consents[0].scope, SpeakerConsentScope::Listen);
    assert_eq!(
        view.consents[0].endpoint_label.as_deref(),
        Some("phone microphone")
    );
    assert_eq!(
        view.consents[0].prompt(),
        "Let desk use this microphone for Living room?"
    );

    let decision = phone
        .core
        .resolve_listen_consent(&fabric, &phone.id(), group, true, &stamp("phone"))
        .unwrap();
    phone.publish(&mut fabric);
    assert!(desk.handle(&fabric, &decision[0], "phone", now).is_empty());
    assert_eq!(
        candidate_state(&desk, &fabric, group, "phone", now),
        ListenerState::Ready
    );
    assert!(
        phone
            .core
            .may_listen(&fabric, &phone.id(), &desk.id(), group, microphone("phone"))
    );
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
            .may_listen(&fabric, &phone.id(), &desk.id(), group, output("phone"))
    );
    let memberships = phone.core.view(&fabric, &phone.id(), &|_| None).memberships;
    assert_eq!(memberships.len(), 1);
    assert_eq!(memberships[0].scope, SpeakerConsentScope::Listen);

    // Withdrawing tells the leader and stops listening at once.
    let withdrawn = phone.core.stop_listening(group).unwrap();
    assert_eq!(
        withdrawn[0].action,
        PersonalFabricAction::SpeakerListenDecision {
            group,
            accepted: false
        }
    );
    assert!(
        !phone
            .core
            .may_listen(&fabric, &phone.id(), &desk.id(), group, microphone("phone"))
    );
}

#[test]
fn a_declined_microphone_is_not_asked_again_until_the_prompt_expires() {
    let now = Instant::now();
    let (mut desk, mut shelf, mut phone) = (
        Device::new("desk"),
        Device::new("shelf"),
        Device::new("phone"),
    );
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    phone.publish(&mut fabric);
    let published = fabric.speaker_group(group).unwrap().clone();
    let asked = [(phone.id(), microphone("phone"))];
    let invites = desk.core.invite_listeners(&published, &asked, now);
    phone.handle(&fabric, &invites[0], "desk", now);
    let decision = phone
        .core
        .resolve_listen_consent(&fabric, &phone.id(), group, false, &stamp("phone"))
        .unwrap();
    desk.handle(&fabric, &decision[0], "phone", now);
    assert_eq!(
        candidate_state(&desk, &fabric, group, "phone", now),
        ListenerState::Declined
    );
    assert!(
        desk.core
            .invite_listeners(&published, &asked, now)
            .is_empty()
    );
    let later = now + CONSENT_TTL;
    assert_eq!(
        candidate_state(&desk, &fabric, group, "phone", later),
        ListenerState::NotAsked
    );
    assert_eq!(
        desk.core.invite_listeners(&published, &asked, later).len(),
        1
    );
}

#[test]
fn microphone_consent_ends_with_the_group_and_with_media_continuity() {
    let now = Instant::now();
    let (mut desk, mut shelf, mut phone) = (
        Device::new("desk"),
        Device::new("shelf"),
        Device::new("phone"),
    );
    let (mut fabric, group) = joined_pair(&mut desk, &mut shelf, now);
    phone.publish(&mut fabric);
    let published = fabric.speaker_group(group).unwrap().clone();
    let consent = |phone: &mut Device, desk: &mut Device, fabric: &mut DeviceFabric, at| {
        let invites =
            desk.core
                .invite_listeners(&published, &[(phone.id(), microphone("phone"))], at);
        assert_eq!(invites.len(), 1);
        for invite in &invites {
            phone.handle(fabric, invite, "desk", now);
        }
        phone
            .core
            .resolve_listen_consent(fabric, &phone.id(), group, true, &stamp("phone"))
            .unwrap();
        phone.publish(fabric);
    };
    consent(&mut phone, &mut desk, &mut fabric, now);
    let stopped = phone.core.pump(&fabric, &phone.id(), false, now);
    assert!(stopped.iter().any(|item| {
        item.target == desk.id()
            && item.action
                == PersonalFabricAction::SpeakerListenDecision {
                    group,
                    accepted: false,
                }
    }));
    assert!(phone.core.listeners.iter().flatten().next().is_none());

    consent(&mut phone, &mut desk, &mut fabric, now + CONSENT_TTL);
    let released = desk.core.delete_group(group).unwrap();
    desk.publish(&mut fabric);
    let leave = released
        .iter()
        .find(|item| item.target == phone.id())
        .expect("the listening device is released too");
    phone.handle(&fabric, leave, "desk", now);
    assert!(phone.core.listeners.iter().flatten().next().is_none());
}
