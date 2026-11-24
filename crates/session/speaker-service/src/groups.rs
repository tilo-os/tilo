//! Speaker groups kept in the record of the person's devices.
//!
//! [`SpeakerGroupCore`] is the whole policy, driven with the record and the live fabric as
//! explicit values, so it is testable without a network or a clock. The process-global wrapper
//! below feeds it what the record and the fabric say, and writes back what it decides.
//!
//! Three things are kept apart:
//!
//! * **Configuration** — the groups a device leads, the outputs and microphones it offers, and
//!   the consents it gave. The record of the person's devices holds each under the device that
//!   wrote it, so every device of theirs reads it and only that device edits it.
//! * **Consent** — a membership another device leads. The member device asks its person once, on
//!   its own screen, and writes the answer into its own part of the record; a change to the
//!   member's output or role no longer matches it, so the member is asked again.
//! * **Playbacks** — one playback to a group. Any of the person's devices may offer one; each
//!   member admits it only while its consent covers the group as its leader wrote it, with a
//!   fresh random capability bound to that exact source and session. Losing the source, the
//!   consent, the group, or media continuity ends it.
//!
//! The timed audio path consumes [`outbound_playback_plans`] on the playing device and
//! [`member_grant`] on each member. `PipeWire` identities stay inside [`resolve_local_output`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tilo_network_actor::fabric::link::FabricLink;
use tilo_resource::Invoke;
use tilo_workspace_service::workspaces::person_workspace::devices::Devices;
use tilo_workspace_service::workspaces::person_workspace::speaker as record;

use tilo_network_actor::fabric::DeviceId;
use tilo_network_actor::fabric::FabricStamp;
use tilo_network_actor::fabric::clock::{ClockSyncFailure, ClockSyncState};
use tilo_network_actor::fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerGroup, SpeakerGroupId,
    SpeakerGroupMember, SpeakerListener, SpeakerMemberStatus, SpeakerMembership, SpeakerRole,
    SpeakerTrim,
};
use tilo_network_actor::fabric::{PersonDevices, PersonalFabricAction};

mod endpoints;
mod listening;

use endpoints::local_endpoints;
pub use listening::{ListenEgress, ListenerCandidate, ListenerState};

/// How long an unanswered consent prompt stays on the member device.
const CONSENT_TTL: Duration = Duration::from_secs(10 * 60);
/// How long a playing device waits for one member to admit a session.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on concurrent sessions in either direction.
pub const MAX_SPEAKER_SESSIONS: usize = 4;

/// One change the policy makes to the record of the person's devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeakerWrite {
    /// The outputs and microphones this device offers.
    Endpoints {
        device: DeviceId,
        endpoints: Vec<AudioEndpoint>,
    },
    /// A group this device leads, as it now stands.
    Lead {
        device: DeviceId,
        group: SpeakerGroup,
    },
    /// A group this device no longer leads.
    Drop {
        device: DeviceId,
        group: SpeakerGroupId,
    },
    /// This device's answer to a group led elsewhere: a consent to play, or to listen.
    Consent {
        device: DeviceId,
        consent: ConsentWrite,
        approved: bool,
    },
    /// This device takes back a consent it gave.
    Withdraw {
        device: DeviceId,
        group: SpeakerGroupId,
        leader: DeviceId,
        play: bool,
    },
    /// Offer one playback of a group to one member.
    Offer {
        route: [u8; 16],
        session: [u8; 16],
        source: DeviceId,
        target: DeviceId,
        group: SpeakerGroupId,
    },
    /// A member's answer to an offer: its capability, or nothing.
    Admit {
        route: [u8; 16],
        authorization: Option<[u8; 16]>,
    },
    /// End a playback, from either end.
    Stop {
        route: [u8; 16],
        authorization: Option<[u8; 16]>,
    },
}

/// What a consent covers: playing in a group, or letting it listen with a microphone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsentWrite {
    Play {
        group: SpeakerGroupId,
        leader: DeviceId,
        output: EndpointSelector,
        role: SpeakerRole,
    },
    Listen {
        group: SpeakerGroupId,
        leader: DeviceId,
        microphone: EndpointSelector,
    },
}

/// What a session plays. The playing device resolves it; it never leaves the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupAudioSource {
    /// An entry of the shell's live-audio source picker.
    PickerToken(u64),
    /// A local media session, by its logical id.
    MediaSession(String),
    /// Everything this device plays: the group is its output.
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMemberPhase {
    /// This device's own output: nothing to ask.
    Local,
    AwaitingAdmission,
    Admitted,
    Refused,
}

#[derive(Clone, Debug)]
struct SessionMember {
    /// The id this member's route travels under: one per member, so each
    /// member's capability and each audio route stay separate.
    route: [u8; 16],
    device: DeviceId,
    output: EndpointSelector,
    channel: media::sync_playout::ChannelRole,
    trim: SpeakerTrim,
    phase: SessionMemberPhase,
    authorization: Option<[u8; 16]>,
}

#[derive(Clone, Debug)]
struct GroupSession {
    group: SpeakerGroupId,
    source: GroupAudioSource,
    members: Vec<SessionMember>,
    started: Instant,
}

#[derive(Clone, Debug)]
struct MemberSession {
    source: DeviceId,
    group: SpeakerGroupId,
    authorization: [u8; 16],
}

#[derive(Clone, Debug)]
struct PendingConsent {
    leader: DeviceId,
    output: EndpointSelector,
    role: SpeakerRole,
    received: Instant,
}

/// A request to edit one member of a group this device leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberSpec {
    pub device: DeviceId,
    pub output: EndpointSelector,
    pub role: SpeakerRole,
}

/// One playback as the record holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedPlayback {
    pub route: [u8; 16],
    pub session: [u8; 16],
    pub source: DeviceId,
    pub target: DeviceId,
    pub group: SpeakerGroupId,
    pub admitted: bool,
    pub refused: bool,
    pub authorization: Option<[u8; 16]>,
}

/// The person's speaker groups as the record of their devices holds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpeakerState {
    /// The groups any of their devices leads, newest write of each id.
    pub groups: Vec<SpeakerGroup>,
    /// What each device offers, by device.
    pub endpoints: BTreeMap<DeviceId, Vec<AudioEndpoint>>,
    /// The consents each device gave to play, by device.
    pub memberships: BTreeMap<DeviceId, Vec<SpeakerMembership>>,
    /// The consents each device gave to listen, by device.
    pub listeners: BTreeMap<DeviceId, Vec<SpeakerListener>>,
    /// The consents each device declined, by device: kept so it is not asked again.
    pub declined: BTreeMap<DeviceId, Vec<record::SpeakerConsent>>,
    /// Every playback the record holds.
    pub playbacks: Vec<RecordedPlayback>,
}

impl SpeakerState {
    /// The record `record` read as speaker groups: entries that do not parse are left out.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one reading keeps every part of the record beside the type it becomes"
    )]
    pub fn of(record: &Devices) -> Self {
        let mut state = Self::default();
        for (key, device) in record.speaker_devices.iter() {
            let Ok(id) = DeviceId::new(key.as_str()) else {
                continue;
            };
            let endpoints = device
                .endpoints
                .iter()
                .filter_map(|endpoint| {
                    Some(AudioEndpoint {
                        selector: EndpointSelector::from_hex(&endpoint.selector)?,
                        kind: if endpoint.microphone {
                            AudioEndpointKind::Microphone
                        } else {
                            AudioEndpointKind::Output
                        },
                        label: endpoint.label.clone(),
                        is_default: endpoint.is_default,
                    })
                })
                .collect::<Vec<_>>();
            let groups = device
                .groups
                .iter()
                .filter_map(|led| {
                    let group_id = SpeakerGroupId::from_hex(&led.id)?;
                    let members = led
                        .members
                        .iter()
                        .filter_map(|member| {
                            Some(SpeakerGroupMember {
                                device: DeviceId::new(&member.device).ok()?,
                                output: EndpointSelector::from_hex(&member.output)?,
                                role: role_from_record(&member.role)?,
                                trim: trim_from_record(member),
                            })
                        })
                        .collect::<Vec<_>>();
                    if members.len() != led.members.len() {
                        return None;
                    }
                    let group = SpeakerGroup {
                        id: group_id,
                        name: led.name.clone(),
                        leader: id.clone(),
                        members,
                        stamp: FabricStamp {
                            counter: u64::try_from(led.at_ms).unwrap_or(0),
                            device: id.clone(),
                        },
                    };
                    group.validate(&group.leader).ok()?;
                    Some(group)
                })
                .collect::<Vec<_>>();
            if !endpoints.is_empty() {
                state.endpoints.insert(id.clone(), endpoints);
            }
            state.groups.extend(groups);
            for consent in &device.consents {
                match (
                    consent.approved,
                    SpeakerGroupId::from_hex(&consent.group),
                    DeviceId::new(&consent.leader).ok(),
                ) {
                    (true, Some(group), Some(leader)) if consent.play => {
                        if let Some(membership) = play_membership(group, leader, consent) {
                            state
                                .memberships
                                .entry(id.clone())
                                .or_default()
                                .push(membership);
                        }
                    }
                    (true, Some(group), Some(leader)) => {
                        if let Some(microphone) = EndpointSelector::from_hex(&consent.microphone) {
                            state
                                .listeners
                                .entry(id.clone())
                                .or_default()
                                .push(SpeakerListener {
                                    group,
                                    leader,
                                    microphone,
                                    stamp: FabricStamp {
                                        counter: u64::try_from(consent.at_ms).unwrap_or(0),
                                        device: id.clone(),
                                    },
                                });
                        }
                    }
                    (false, _, _) => {
                        state
                            .declined
                            .entry(id.clone())
                            .or_default()
                            .push(consent.clone());
                    }
                    _ => {}
                }
            }
        }
        state
            .groups
            .sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        for (key, playback) in record.speaker_playbacks.iter() {
            let (Some(route), Some(session), Some(group)) = (
                selector_bytes(key.as_str()),
                selector_bytes(playback.session.as_str()),
                SpeakerGroupId::from_hex(&playback.group),
            ) else {
                continue;
            };
            let (Ok(source), Ok(target)) = (
                DeviceId::new(&playback.source),
                DeviceId::new(&playback.target),
            ) else {
                continue;
            };
            let phase = playback.stage;
            state.playbacks.push(RecordedPlayback {
                route,
                session,
                source,
                target,
                group,
                admitted: phase == record::PlaybackStage::Admitted,
                refused: phase == record::PlaybackStage::Refused,
                authorization: EndpointSelector::from_hex(&playback.authorization)
                    .map(|selector| selector.0),
            });
        }
        state
    }

    /// The group `id`, as its leader wrote it.
    #[must_use]
    pub fn group(&self, id: SpeakerGroupId) -> Option<&SpeakerGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    /// What `device` offers.
    #[must_use]
    pub fn endpoints_of(&self, device: &DeviceId) -> &[AudioEndpoint] {
        self.endpoints.get(device).map_or(&[], Vec::as_slice)
    }

    /// The output `device` offers under `selector`.
    #[must_use]
    pub fn output(&self, device: &DeviceId, selector: EndpointSelector) -> Option<&AudioEndpoint> {
        self.endpoints_of(device).iter().find(|endpoint| {
            endpoint.selector == selector && endpoint.kind == AudioEndpointKind::Output
        })
    }

    /// The microphone `device` offers under `selector`.
    #[must_use]
    pub fn microphone(
        &self,
        device: &DeviceId,
        selector: EndpointSelector,
    ) -> Option<&AudioEndpoint> {
        self.endpoints_of(device).iter().find(|endpoint| {
            endpoint.selector == selector && endpoint.kind == AudioEndpointKind::Microphone
        })
    }

    /// The microphone calibration listens with on `device`: the default one, else the first.
    #[must_use]
    pub fn calibration_microphone(&self, device: &DeviceId) -> Option<&AudioEndpoint> {
        let mut microphones = self
            .endpoints_of(device)
            .iter()
            .filter(|endpoint| endpoint.kind == AudioEndpointKind::Microphone);
        let first = microphones.clone().next();
        microphones.find(|endpoint| endpoint.is_default).or(first)
    }

    /// The consents `device` gave to play in groups.
    #[must_use]
    pub fn memberships_of(&self, device: &DeviceId) -> &[SpeakerMembership] {
        self.memberships.get(device).map_or(&[], Vec::as_slice)
    }

    /// The consents `device` gave to let groups listen.
    #[must_use]
    pub fn listeners_of(&self, device: &DeviceId) -> &[SpeakerListener] {
        self.listeners.get(device).map_or(&[], Vec::as_slice)
    }

    /// Whether `device`'s consent covers `member` of `group` as it stands.
    #[must_use]
    pub fn membership_covers(
        &self,
        device: &DeviceId,
        group: &SpeakerGroup,
        member: &SpeakerGroupMember,
    ) -> bool {
        self.memberships_of(device)
            .iter()
            .any(|membership| membership.covers(group, member))
    }

    /// Whether `device` declined to play `member` of `group` as it now stands, so it is not asked
    /// again until the leader changes it.
    #[must_use]
    pub fn declined_play(
        &self,
        device: &DeviceId,
        group: &SpeakerGroup,
        member: &SpeakerGroupMember,
    ) -> bool {
        self.declined.get(device).is_some_and(|declined| {
            declined.iter().any(|consent| {
                consent.play
                    && SpeakerGroupId::from_hex(&consent.group) == Some(group.id)
                    && DeviceId::new(&consent.leader).ok().as_ref() == Some(&group.leader)
                    && EndpointSelector::from_hex(&consent.output) == Some(member.output)
                    && role_from_record(&consent.role) == Some(member.role)
            })
        })
    }

    /// Where each member of `group` stands, in member order.
    #[must_use]
    pub fn member_states(
        &self,
        fabric: &PersonDevices,
        group: &SpeakerGroup,
    ) -> Vec<SpeakerMemberStatus> {
        group
            .members
            .iter()
            .map(|member| {
                let Some(device) = fabric.get(&member.device) else {
                    return SpeakerMemberStatus::NotCertified;
                };
                if member.device != group.leader && !device.about.capabilities.audio_group {
                    return SpeakerMemberStatus::Unavailable;
                }
                if self.output(&member.device, member.output).is_none() {
                    return SpeakerMemberStatus::OutputMissing;
                }
                let consented = member.device == group.leader
                    || self.membership_covers(&member.device, group, member);
                if !consented {
                    SpeakerMemberStatus::AwaitingConsent
                } else if device.online {
                    SpeakerMemberStatus::Ready
                } else {
                    SpeakerMemberStatus::Offline
                }
            })
            .collect()
    }

    /// Whether every member is one of the person's devices, can join speaker groups (the
    /// leader's own outputs always can), and offers the named output.
    ///
    /// # Errors
    /// The group does not hold, names a device that is not the person's, or names an output that
    /// device does not offer.
    pub fn admits(&self, fabric: &PersonDevices, group: &SpeakerGroup) -> Result<(), String> {
        group
            .validate(&group.leader)
            .map_err(|error| error.to_string())?;
        for member in &group.members {
            let device = fabric
                .get(&member.device)
                .ok_or_else(|| crate::i18n::t("speaker-error-device-unknown"))?;
            if (member.device != group.leader && !device.about.capabilities.audio_group)
                || self.output(&member.device, member.output).is_none()
            {
                return Err(crate::i18n::t("speaker-error-member-unavailable"));
            }
        }
        Ok(())
    }

    /// The playback whose route is `route`.
    #[must_use]
    pub fn playback(&self, route: [u8; 16]) -> Option<&RecordedPlayback> {
        self.playbacks
            .iter()
            .find(|playback| playback.route == route)
    }
}

fn role_from_record(value: &str) -> Option<SpeakerRole> {
    use tilo_network_actor::fabric::speaker_group::{RoomPoint, StereoSide, SurroundChannel};
    Some(match value {
        "left" => SpeakerRole::Stereo(StereoSide::Left),
        "right" => SpeakerRole::Stereo(StereoSide::Right),
        "center" => SpeakerRole::Surround(SurroundChannel::Center),
        "lfe" => SpeakerRole::Surround(SurroundChannel::Lfe),
        "surround-left" => SpeakerRole::Surround(SurroundChannel::SurroundLeft),
        "surround-right" => SpeakerRole::Surround(SurroundChannel::SurroundRight),
        point => {
            let (x_mm, y_mm, z_mm) = {
                let mut parts = point.strip_prefix("room:")?.split(':');
                let x = parts.next()?.parse().ok()?;
                let y = parts.next()?.parse().ok()?;
                let z = parts.next()?.parse().ok()?;
                (x, y, z)
            };
            SpeakerRole::RoomPoint(RoomPoint { x_mm, y_mm, z_mm })
        }
    })
}

fn role_to_record(role: SpeakerRole) -> String {
    use tilo_network_actor::fabric::speaker_group::{StereoSide, SurroundChannel};
    match role {
        SpeakerRole::Stereo(StereoSide::Left) => "left".to_owned(),
        SpeakerRole::Stereo(StereoSide::Right) => "right".to_owned(),
        SpeakerRole::Surround(SurroundChannel::Center) => "center".to_owned(),
        SpeakerRole::Surround(SurroundChannel::Lfe) => "lfe".to_owned(),
        SpeakerRole::Surround(SurroundChannel::SurroundLeft) => "surround-left".to_owned(),
        SpeakerRole::Surround(SurroundChannel::SurroundRight) => "surround-right".to_owned(),
        SpeakerRole::RoomPoint(point) => {
            format!("room:{}:{}:{}", point.x_mm, point.y_mm, point.z_mm)
        }
    }
}

fn trim_from_record(member: &record::SpeakerLedMember) -> SpeakerTrim {
    SpeakerTrim {
        delay_ns: member.delay_ns,
        gain_mb: member.gain_mb,
        eq: member
            .eq
            .iter()
            .map(
                |band| tilo_network_actor::fabric::speaker_group::SpeakerEqBand {
                    center_hz: band.center_hz,
                    gain_mb: band.gain_mb,
                    q_milli: band.q_milli,
                },
            )
            .collect(),
    }
}

fn trim_to_record(trim: &SpeakerTrim) -> (u64, i32, Vec<record::SpeakerEqBand>) {
    (
        trim.delay_ns,
        trim.gain_mb,
        trim.eq
            .iter()
            .map(|band| record::SpeakerEqBand {
                center_hz: band.center_hz,
                gain_mb: band.gain_mb,
                q_milli: band.q_milli,
            })
            .collect(),
    )
}

fn selector_bytes(value: &str) -> Option<[u8; 16]> {
    let mut bytes = [0; 16];
    (hex::decode_to_slice(value, &mut bytes).is_ok() && hex::encode(bytes) == value)
        .then_some(bytes)
}

fn play_membership(
    group: SpeakerGroupId,
    leader: DeviceId,
    consent: &record::SpeakerConsent,
) -> Option<SpeakerMembership> {
    Some(SpeakerMembership {
        group,
        leader: leader.clone(),
        output: EndpointSelector::from_hex(&consent.output)?,
        role: role_from_record(&consent.role)?,
        stamp: FabricStamp {
            counter: u64::try_from(consent.at_ms).unwrap_or(0),
            device: leader,
        },
    })
}

/// The speaker-group policy, independent of transport and wall time.
#[derive(Default)]
pub struct SpeakerGroupCore {
    /// Consents this device was asked for and has not answered.
    consents: BTreeMap<SpeakerGroupId, PendingConsent>,
    /// Microphone consents it was asked for and has not answered.
    listen_consents: BTreeMap<SpeakerGroupId, listening::PendingListen>,
    /// Microphone invites it sent, and their answers.
    listen_invites: BTreeMap<(SpeakerGroupId, DeviceId), listening::ListenInvite>,
    outbound: BTreeMap<[u8; 16], GroupSession>,
    inbound: BTreeMap<[u8; 16], MemberSession>,
    /// Media continuity was turned off; nothing starts until it is back.
    media_off: bool,
}

impl SpeakerGroupCore {
    /// Create a group this device leads.
    pub fn create_group(
        &mut self,
        state: &SpeakerState,
        fabric: &PersonDevices,
        local: &DeviceId,
        id: SpeakerGroupId,
        name: &str,
        members: &[MemberSpec],
        now: Instant,
    ) -> Result<Vec<SpeakerWrite>, String> {
        if state
            .groups
            .iter()
            .filter(|group| &group.leader == local)
            .count()
            >= tilo_network_actor::fabric::speaker_group::MAX_SPEAKER_GROUPS
        {
            return Err(crate::i18n::t("speaker-error-too-many-groups"));
        }
        if state.group(id).is_some() {
            return Err(crate::i18n::t("speaker-error-group-exists"));
        }
        let group = build_group(id, name, local, members, &BTreeMap::new())?;
        state.admits(fabric, &group)?;
        let _ = now;
        Ok(vec![SpeakerWrite::Lead {
            device: local.clone(),
            group,
        }])
    }

    /// Replace the name and members of a group this device leads. Members keep their calibration
    /// while their output and role stay the same; removed members are let go.
    pub fn update_group(
        &mut self,
        state: &SpeakerState,
        fabric: &PersonDevices,
        local: &DeviceId,
        id: SpeakerGroupId,
        name: &str,
        members: &[MemberSpec],
        now: Instant,
    ) -> Result<Vec<SpeakerWrite>, String> {
        let previous = Self::led_group(state, local, id)
            .cloned()
            .ok_or_else(|| crate::i18n::t("speaker-error-group-gone"))?;
        let trims = previous
            .members
            .iter()
            .map(|member| {
                (
                    (member.device.clone(), member.output, member.role),
                    member.trim.clone(),
                )
            })
            .collect();
        let group = build_group(id, name, local, members, &trims)?;
        state.admits(fabric, &group)?;
        let mut out = Vec::new();
        for device in removed_members(&previous, &group) {
            out.extend(self.end_outbound_member(|session, member| {
                session.group == id && member.device == device
            }));
        }
        out.push(SpeakerWrite::Lead {
            device: local.clone(),
            group,
        });
        out.extend(self.end_outbound_for_group_if_unchanged(id));
        let _ = now;
        Ok(out)
    }

    /// Replace the calibration trims of a led group, member by member. The member set is
    /// unchanged, so no consent is asked again.
    pub fn set_trims(
        &mut self,
        state: &SpeakerState,
        local: &DeviceId,
        id: SpeakerGroupId,
        trims: &[(DeviceId, EndpointSelector, SpeakerTrim)],
    ) -> Result<Vec<SpeakerWrite>, String> {
        let group = Self::led_group(state, local, id)
            .cloned()
            .ok_or_else(|| crate::i18n::t("speaker-error-group-gone"))?;
        let mut updated = group;
        for (device, output, trim) in trims {
            let member = updated
                .members
                .iter_mut()
                .find(|member| &member.device == device && member.output == *output)
                .ok_or_else(|| crate::i18n::t("speaker-error-not-in-group"))?;
            member.trim = trim.clone();
        }
        updated
            .validate(local)
            .map_err(|_| crate::i18n::t("speaker-error-calibration-range"))?;
        Ok(vec![SpeakerWrite::Lead {
            device: local.clone(),
            group: updated,
        }])
    }

    /// Delete a group this device leads and release every member.
    pub fn delete_group(&mut self, local: &DeviceId, id: SpeakerGroupId) -> Vec<SpeakerWrite> {
        let mut out = self.end_outbound_for_group(id);
        out.push(SpeakerWrite::Drop {
            device: local.clone(),
            group: id,
        });
        out
    }

    /// Answer a join prompt on this device.
    pub fn resolve_consent(
        &mut self,
        state: &SpeakerState,
        local: &DeviceId,
        group_id: SpeakerGroupId,
        approved: bool,
        now: Instant,
    ) -> Result<Vec<SpeakerWrite>, String> {
        let pending = self
            .consents
            .remove(&group_id)
            .ok_or_else(|| crate::i18n::t("speaker-error-request-expired"))?;
        if approved {
            let published = state
                .group(group_id)
                .filter(|published| published.leader == pending.leader)
                .ok_or_else(|| crate::i18n::t("speaker-error-group-changed"))?;
            let member = published
                .member(local)
                .filter(|member| member.output == pending.output && member.role == pending.role)
                .ok_or_else(|| crate::i18n::t("speaker-error-group-changed"))?;
            if state.memberships_of(local).len()
                >= tilo_network_actor::fabric::speaker_group::MAX_SPEAKER_MEMBERSHIPS
            {
                self.consents.insert(group_id, pending);
                return Err(crate::i18n::t("speaker-error-too-many-memberships"));
            }
            let _ = member;
        }
        let _ = now;
        Ok(vec![SpeakerWrite::Consent {
            device: local.clone(),
            consent: ConsentWrite::Play {
                group: group_id,
                leader: pending.leader,
                output: pending.output,
                role: pending.role,
            },
            approved,
        }])
    }

    /// Withdraw this device's consent to a group led elsewhere.
    pub fn leave(
        &mut self,
        state: &SpeakerState,
        local: &DeviceId,
        group_id: SpeakerGroupId,
    ) -> Result<Vec<SpeakerWrite>, String> {
        let membership = state
            .memberships_of(local)
            .iter()
            .find(|membership| membership.group == group_id)
            .ok_or_else(|| crate::i18n::t("speaker-error-not-a-member"))?
            .clone();
        let mut out = vec![SpeakerWrite::Withdraw {
            device: local.clone(),
            group: group_id,
            leader: membership.leader,
            play: true,
        }];
        out.extend(self.end_inbound(|session| session.group == group_id));
        Ok(out)
    }

    /// Start playing `source` to a group from this device.
    pub fn start_session(
        &mut self,
        state: &SpeakerState,
        fabric: &PersonDevices,
        local: &DeviceId,
        group_id: SpeakerGroupId,
        source: GroupAudioSource,
        session_id: [u8; 16],
        now: Instant,
    ) -> Result<Vec<SpeakerWrite>, String> {
        if self.media_off {
            return Err(crate::i18n::t("speaker-error-media-off"));
        }
        if self.outbound.len().saturating_add(self.inbound.len()) >= MAX_SPEAKER_SESSIONS {
            return Err(crate::i18n::t("speaker-error-too-many-sessions"));
        }
        let group = state
            .group(group_id)
            .ok_or_else(|| crate::i18n::t("speaker-error-group-unavailable"))?
            .clone();
        let states = state.member_states(fabric, &group);
        let channels = group.channel_roles();
        let members = group
            .members
            .iter()
            .zip(states)
            .zip(channels)
            .filter(|((_, state), _)| *state == SpeakerMemberStatus::Ready)
            .map(|((member, _), channel)| SessionMember {
                route: member_route_id(session_id, &member.device, member.output),
                device: member.device.clone(),
                output: member.output,
                channel,
                trim: member.trim.clone(),
                phase: if &member.device == local {
                    SessionMemberPhase::Local
                } else {
                    SessionMemberPhase::AwaitingAdmission
                },
                authorization: None,
            })
            .collect::<Vec<_>>();
        if members.is_empty() {
            return Err(crate::i18n::t("speaker-error-none-ready"));
        }
        let offers = members
            .iter()
            .filter(|member| member.phase == SessionMemberPhase::AwaitingAdmission)
            .map(|member| SpeakerWrite::Offer {
                route: member.route,
                session: session_id,
                source: local.clone(),
                target: member.device.clone(),
                group: group_id,
            })
            .collect();
        self.outbound.insert(
            session_id,
            GroupSession {
                group: group_id,
                source,
                members,
                started: now,
            },
        );
        Ok(offers)
    }

    /// Stop one session in either direction.
    pub fn stop_session(&mut self, session_id: [u8; 16]) -> Result<Vec<SpeakerWrite>, String> {
        if let Some(session) = self.outbound.remove(&session_id) {
            return Ok(stop_members(&session));
        }
        if let Some(session) = self.inbound.remove(&session_id) {
            return Ok(vec![SpeakerWrite::Stop {
                route: session_id,
                authorization: Some(session.authorization),
            }]);
        }
        Err(crate::i18n::t("speaker-error-session-gone"))
    }

    /// Consume one authenticated live speaker action: the writes it makes to the record, and
    /// the live asks it answers. `None` for any other action.
    pub fn handle(
        &mut self,
        state: &SpeakerState,
        local: &DeviceId,
        source: &DeviceId,
        action: &PersonalFabricAction,
        now: Instant,
    ) -> Option<(Vec<SpeakerWrite>, Vec<ListenEgress>)> {
        self.handle_listen_action(state, local, source, action, now)
            .map(|live| (Vec::new(), live))
    }

    /// Whether this device may admit a playlist for `group` right now: the leader writes the
    /// group with this device as a member, and either this device is that leader or its consent
    /// covers the member as published.
    fn consent_covers(
        state: &SpeakerState,
        local: &DeviceId,
        group: SpeakerGroupId,
    ) -> bool {
        let Some(published) = state.group(group) else {
            return false;
        };
        let Some(member) = published.member(local) else {
            return false;
        };
        &published.leader == local || state.membership_covers(local, published, member)
    }

    /// Periodic maintenance: what the record says about this device's groups, consents and
    /// playbacks now, and the writes that bring the two in line. With media continuity off
    /// everything ends and consents are withdrawn; otherwise the record's new groups are asked
    /// about, offers are admitted or refused, and stale sessions end.
    pub fn reconcile(
        &mut self,
        state: &SpeakerState,
        fabric: &PersonDevices,
        local: &DeviceId,
        enabled: bool,
        now: Instant,
        mint: &mut dyn FnMut() -> [u8; 16],
    ) -> Vec<SpeakerWrite> {
        let mut out = self.pump_listening(state, fabric, local, enabled, now);
        self.media_off = !enabled;
        if !enabled {
            out.extend(self.end_inbound(|_| true));
            for session in std::mem::take(&mut self.outbound).into_values() {
                out.extend(stop_members(&session));
            }
            self.consents.clear();
            self.listen_consents.clear();
            self.listen_invites.clear();
            for membership in state.memberships_of(local) {
                out.push(SpeakerWrite::Withdraw {
                    device: local.clone(),
                    group: membership.group,
                    leader: membership.leader.clone(),
                    play: true,
                });
            }
            for listener in state.listeners_of(local) {
                out.push(SpeakerWrite::Withdraw {
                    device: local.clone(),
                    group: listener.group,
                    leader: listener.leader.clone(),
                    play: false,
                });
            }
            return out;
        }

        // A group led elsewhere that names this device and that it neither agreed to nor
        // declined as it now stands is asked about here, once.

        for group in state.groups.iter().filter(|group| &group.leader != local) {
            let Some(member) = group.member(local) else {
                continue;
            };
            if state.membership_covers(local, group, member)
                || state.declined_play(local, group, member)
                || self.consents.get(&group.id).is_some_and(|pending| {
                    pending.leader == group.leader
                        && pending.output == member.output
                        && pending.role == member.role
                })
            {
                continue;
            }
            self.consents.insert(
                group.id,
                PendingConsent {
                    leader: group.leader.clone(),
                    output: member.output,
                    role: member.role,
                    received: now,
                },
            );
        }

        // A consent whose group its leader no longer writes as it was agreed is void.
        let voided = state
            .memberships_of(local)
            .iter()
            .filter(|membership| {
                fabric.contains(&membership.leader)
                    && !state.group(membership.group).is_some_and(|group| {
                        group
                            .member(local)
                            .is_some_and(|member| membership.covers(group, member))
                    })
            })
            .map(|membership| membership.group)
            .collect::<BTreeSet<_>>();
        for group in voided {
            let leader = state
                .memberships_of(local)
                .iter()
                .find(|membership| membership.group == group)
                .map(|membership| membership.leader.clone());
            if let Some(leader) = leader {
                out.push(SpeakerWrite::Withdraw {
                    device: local.clone(),
                    group,
                    leader,
                    play: true,
                });
            }
        }

        self.consents.retain(|group, pending| {
            now.saturating_duration_since(pending.received) < CONSENT_TTL
                && fabric
                    .get(&pending.leader)
                    .is_some_and(|leader| leader.online)
                && state.group(*group).is_some_and(|group| {
                    group.leader == pending.leader
                        && group.member(local).is_some_and(|member| {
                            member.output == pending.output && member.role == pending.role
                        })
                })
        });

        out.extend(self.reconcile_outbound(state, fabric, local, now, mint));
        out.extend(self.reconcile_inbound(state, fabric, local, now, mint));
        out
    }

    /// Bring each offered playback to this device in line with the record: a member that
    /// consents admits it with a fresh capability, one that does not refuses it, and the audio
    /// path plays what is admitted.
    fn reconcile_inbound(
        &mut self,
        state: &SpeakerState,
        fabric: &PersonDevices,
        local: &DeviceId,
        now: Instant,
        mint: &mut dyn FnMut() -> [u8; 16],
    ) -> Vec<SpeakerWrite> {
        let mut out = Vec::new();
        let mut keep = BTreeSet::new();
        for playback in state
            .playbacks
            .iter()
            .filter(|playback| &playback.target == local)
        {
            keep.insert(playback.route);
            let online = fabric
                .get(&playback.source)
                .is_some_and(|device| device.online);
            let covers = Self::consent_covers(state, local, playback.group);
            if playback.admitted {
                if let Some(authorization) = playback.authorization.filter(|_| covers && online) {
                    let group = playback.group;
                    let source = playback.source.clone();
                    self.inbound.insert(
                        playback.route,
                        MemberSession {
                            source,
                            group,
                            authorization,
                        },
                    );
                    continue;
                }
                self.inbound.remove(&playback.route);
                out.push(SpeakerWrite::Stop {
                    route: playback.route,
                    authorization: playback.authorization,
                });
                continue;
            }
            if playback.refused {
                continue;
            }
            if self.inbound.len().saturating_add(self.outbound.len()) >= MAX_SPEAKER_SESSIONS
                || !covers
            {
                out.push(SpeakerWrite::Admit {
                    route: playback.route,
                    authorization: None,
                });
                continue;
            }
            let authorization = mint();
            self.inbound.insert(
                playback.route,
                MemberSession {
                    source: playback.source.clone(),
                    group: playback.group,
                    authorization,
                },
            );
            out.push(SpeakerWrite::Admit {
                route: playback.route,
                authorization: Some(authorization),
            });
        }
        let ended = self
            .inbound
            .keys()
            .copied()
            .filter(|route| !keep.contains(route))
            .collect::<BTreeSet<_>>();
        out.extend(self.end_inbound_ids(&ended));
        let _ = now;
        out
    }

    /// Bring each session this device plays in line with the record: an admitted member starts
    /// its route, a refused one is let go, and a member whose playback the record lost is
    /// stopped.
    fn reconcile_outbound(
        &mut self,
        state: &SpeakerState,
        fabric: &PersonDevices,
        local: &DeviceId,
        now: Instant,
        _mint: &mut dyn FnMut() -> [u8; 16],
    ) -> Vec<SpeakerWrite> {
        let mut out = Vec::new();
        for session in self.outbound.values_mut() {
            for member in &mut session.members {
                if matches!(
                    member.phase,
                    SessionMemberPhase::Local
                        | SessionMemberPhase::Admitted
                        | SessionMemberPhase::Refused
                ) {
                    continue;
                }
                match state.playback(member.route) {
                    Some(playback) if playback.admitted => {
                        member.authorization = playback.authorization;
                        member.phase = SessionMemberPhase::Admitted;
                    }
                    Some(playback) if playback.refused => {
                        member.phase = SessionMemberPhase::Refused;
                    }
                    _ => {
                        let online = fabric
                            .get(&member.device)
                            .is_some_and(|device| device.online);
                        if !online
                            || now.saturating_duration_since(session.started) >= ADMISSION_TIMEOUT
                        {
                            member.phase = SessionMemberPhase::Refused;
                        }
                    }
                }
            }
        }
        // A member that has not admitted a playback by the deadline is let go, and one that
        // admitted it leaves when the record loses it or the member goes away. An offer whose
        // write has not reached this device's reading yet is still being admitted.
        out.extend(self.drop_outbound_member(|session, member| {
            match member.phase {
                SessionMemberPhase::Local | SessionMemberPhase::Refused => false,
                SessionMemberPhase::AwaitingAdmission => {
                    now.saturating_duration_since(session.started) >= ADMISSION_TIMEOUT
                }
                SessionMemberPhase::Admitted => {
                    state.playback(member.route).is_none()
                        || !fabric
                            .get(&member.device)
                            .is_some_and(|device| device.online)
                }
            }
        }));
        let finished = self
            .outbound
            .iter()
            .filter(|(_, session)| {
                state.group(session.group).is_none()
                    || session
                        .members
                        .iter()
                        .all(|member| member.phase == SessionMemberPhase::Refused)
            })
            .map(|(session_id, _)| *session_id)
            .collect::<Vec<_>>();
        for session_id in finished {
            if let Some(session) = self.outbound.remove(&session_id) {
                out.extend(stop_members(&session));
            }
        }
        let _ = local;
        out
    }

    fn led_group<'a>(
        state: &'a SpeakerState,
        local: &DeviceId,
        id: SpeakerGroupId,
    ) -> Option<&'a SpeakerGroup> {
        state.group(id).filter(|group| &group.leader == local)
    }

    fn end_inbound(
        &mut self,
        mut predicate: impl FnMut(&MemberSession) -> bool,
    ) -> Vec<SpeakerWrite> {
        let ids = self
            .inbound
            .iter()
            .filter(|(_, session)| predicate(session))
            .map(|(session_id, _)| *session_id)
            .collect::<BTreeSet<_>>();
        self.end_inbound_ids(&ids)
    }

    fn end_inbound_ids(&mut self, ids: &BTreeSet<[u8; 16]>) -> Vec<SpeakerWrite> {
        ids.iter()
            .filter_map(|session_id| {
                self.inbound
                    .remove(session_id)
                    .map(|session| SpeakerWrite::Stop {
                        route: *session_id,
                        authorization: Some(session.authorization),
                    })
            })
            .collect()
    }

    fn end_outbound_for_group(&mut self, group: SpeakerGroupId) -> Vec<SpeakerWrite> {
        let (ended, kept): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut self.outbound)
            .into_iter()
            .partition(|(_, session)| session.group == group);
        self.outbound = kept;
        ended.values().flat_map(stop_members).collect()
    }

    fn end_outbound_for_group_if_unchanged(&mut self, group: SpeakerGroupId) -> Vec<SpeakerWrite> {
        self.end_outbound_for_group(group)
    }

    fn end_outbound_member(
        &mut self,
        mut predicate: impl FnMut(&GroupSession, &SessionMember) -> bool,
    ) -> Vec<SpeakerWrite> {
        self.drop_outbound_member(|session, member| predicate(session, member))
    }

    fn drop_outbound_member(
        &mut self,
        mut predicate: impl FnMut(&GroupSession, &SessionMember) -> bool,
    ) -> Vec<SpeakerWrite> {
        let mut out = Vec::new();
        for session in self.outbound.values_mut() {
            let current = session.clone();
            for member in &mut session.members {
                if member.phase == SessionMemberPhase::Refused || !predicate(&current, member) {
                    continue;
                }
                if member.phase != SessionMemberPhase::Local {
                    out.push(SpeakerWrite::Stop {
                        route: member.route,
                        authorization: member.authorization,
                    });
                }
                member.phase = SessionMemberPhase::Refused;
                member.authorization = None;
            }
        }
        out
    }

    /// Presentation rows for the Devices surface and Settings. `playout` answers, per member
    /// route, whether that member can play in time (see
    /// [`crate::remote_audio::group_route_playout`]).
    #[must_use]
    pub fn view(
        &self,
        state: &SpeakerState,
        fabric: &PersonDevices,
        local: &DeviceId,
        playout: &dyn Fn([u8; 16]) -> Option<ClockSyncState>,
    ) -> SpeakerGroupsView {
        let label = |device: &DeviceId| {
            fabric.get(device).map_or_else(
                || device.as_str().to_string(),
                |known| known.about.label.clone(),
            )
        };
        let output_label = |device: &DeviceId, output: EndpointSelector| {
            state
                .output(device, output)
                .map(|endpoint| endpoint.label.clone())
        };
        let groups = state
            .groups
            .iter()
            .map(|group| SpeakerGroupView {
                id: group.id,
                name: group.name.clone(),
                leader: group.leader.clone(),
                leader_label: label(&group.leader),
                led_locally: &group.leader == local,
                members: group
                    .members
                    .iter()
                    .zip(state.member_states(fabric, group))
                    .map(|(member, state)| SpeakerMemberView {
                        device: member.device.clone(),
                        device_label: label(&member.device),
                        output: member.output,
                        output_label: output_label(&member.device, member.output),
                        role: member.role,
                        trim: member.trim.clone(),
                        state,
                    })
                    .collect(),
            })
            .collect();
        let consents = self
            .consents
            .iter()
            .filter(|(group, pending)| consent_matches(state, local, **group, pending))
            .filter_map(|(group, pending)| {
                let published = state.group(*group)?;
                Some(SpeakerConsentView {
                    group: *group,
                    name: published.name.clone(),
                    leader: pending.leader.clone(),
                    leader_label: label(&pending.leader),
                    scope: SpeakerConsentScope::Play(pending.role),
                    endpoint_label: output_label(local, pending.output),
                })
            })
            .chain(self.listen_consent_views(state, fabric, local))
            .collect();
        let memberships = state
            .memberships_of(local)
            .iter()
            .map(|membership| SpeakerMembershipView {
                group: membership.group,
                name: state
                    .group(membership.group)
                    .map_or_else(String::new, |group| group.name.clone()),
                leader_label: label(&membership.leader),
                scope: SpeakerConsentScope::Play(membership.role),
            })
            .chain(self.listener_views(state, fabric, local))
            .collect();
        let sessions = self.session_views(state, &label, playout);
        SpeakerGroupsView {
            groups,
            consents,
            memberships,
            sessions,
        }
    }

    /// One row per session this device plays and per session it admitted, with each session's
    /// members counted by how far their clock has got.
    fn session_views(
        &self,
        state: &SpeakerState,
        label: &dyn Fn(&DeviceId) -> String,
        playout: &dyn Fn([u8; 16]) -> Option<ClockSyncState>,
    ) -> Vec<SpeakerSessionView> {
        let name_of = |group: SpeakerGroupId| {
            state
                .group(group)
                .map_or_else(String::new, |group| group.name.clone())
        };
        self.outbound
            .iter()
            .map(|(session_id, session)| {
                let mut clocks = SessionClocks::default();
                for member in &session.members {
                    match member.phase {
                        SessionMemberPhase::Local => clocks.count(ClockSyncState::Ready),
                        SessionMemberPhase::Admitted => {
                            clocks.count(playout(member.route).unwrap_or(ClockSyncState::Syncing));
                        }
                        SessionMemberPhase::AwaitingAdmission | SessionMemberPhase::Refused => {}
                    }
                }
                SpeakerSessionView {
                    session_id: *session_id,
                    group: session.group,
                    name: name_of(session.group),
                    inbound: false,
                    peer_label: None,
                    playing_members: clocks.ready,
                    waiting_members: session
                        .members
                        .iter()
                        .filter(|member| member.phase == SessionMemberPhase::AwaitingAdmission)
                        .count(),
                    syncing_members: clocks.syncing,
                    failed_members: clocks.failed,
                    clock_failure: clocks.failure,
                }
            })
            .chain(self.inbound.iter().map(|(session_id, session)| {
                let mut clocks = SessionClocks::default();
                clocks.count(playout(*session_id).unwrap_or(ClockSyncState::Syncing));
                SpeakerSessionView {
                    session_id: *session_id,
                    group: session.group,
                    name: name_of(session.group),
                    inbound: true,
                    peer_label: Some(label(&session.source)),
                    playing_members: clocks.ready,
                    waiting_members: 0,
                    syncing_members: clocks.syncing,
                    failed_members: clocks.failed,
                    clock_failure: clocks.failure,
                }
            }))
            .collect()
    }

    /// What the playing device's timed audio path renders for each session: only members that
    /// are this device's own outputs or have admitted it.
    #[must_use]
    pub fn playback_plans(&self) -> Vec<GroupPlaybackPlan> {
        self.outbound
            .iter()
            .map(|(session_id, session)| GroupPlaybackPlan {
                session_id: *session_id,
                group: session.group,
                source: session.source.clone(),
                members: session
                    .members
                    .iter()
                    .filter(|member| {
                        matches!(
                            member.phase,
                            SessionMemberPhase::Local | SessionMemberPhase::Admitted
                        )
                    })
                    .map(|member| PlannedMember {
                        route: member.route,
                        device: member.device.clone(),
                        output: member.output,
                        channel: member.channel,
                        trim: member.trim.clone(),
                        authorization: member.authorization,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Every audio route this device should run right now, derived from the sessions it plays
    /// and the sessions it admitted as a member.
    #[must_use]
    pub fn route_specs(&self, state: &SpeakerState, local: &DeviceId) -> Vec<RouteSpec> {
        let playing = self.outbound.values().flat_map(|session| {
            session
                .members
                .iter()
                .filter_map(|member| match (member.phase, member.authorization) {
                    (SessionMemberPhase::Local, _) => Some(RouteSpec::Local {
                        route: member.route,
                        source: session.source.clone(),
                        output: member.output,
                        channel: member.channel,
                        trim: member.trim.clone(),
                    }),
                    (SessionMemberPhase::Admitted, Some(authorization)) => Some(RouteSpec::Send {
                        route: member.route,
                        target: member.device.clone(),
                        authorization,
                        source: session.source.clone(),
                    }),
                    _ => None,
                })
        });
        let receiving = self.inbound.iter().filter_map(|(route, session)| {
            let grant = self.grant(state, local, *route, &session.source)?;
            Some(RouteSpec::Receive {
                route: *route,
                source: session.source.clone(),
                authorization: grant.authorization,
                output: grant.output,
                channel: grant.channel,
                trim: grant.trim,
            })
        });
        playing.chain(receiving).collect()
    }

    /// The output this device plays a calibration sweep on when `source` asks: only the group's
    /// leader may ask, and only while this device's consent covers its membership.
    #[must_use]
    pub fn sweep_output(
        &self,
        state: &SpeakerState,
        local: &DeviceId,
        source: &DeviceId,
        group: SpeakerGroupId,
    ) -> Option<EndpointSelector> {
        let published = state.group(group)?;
        (&published.leader == source && Self::consent_covers(state, local, group))
            .then(|| published.member(local).map(|member| member.output))?
    }

    /// Whether this device admitted `route` from exactly `source`. Checked for every received
    /// frame, so a withdrawn consent stops audio at once.
    #[must_use]
    pub fn admits(&self, route: [u8; 16], source: &DeviceId) -> bool {
        self.inbound
            .get(&route)
            .is_some_and(|session| &session.source == source)
    }

    /// The audio path ended `route` on its own (a lost link or a failed output): end the member
    /// session it carried.
    pub fn route_lost(&mut self, route: [u8; 16]) -> Vec<SpeakerWrite> {
        if self.inbound.contains_key(&route) {
            return self.end_inbound_ids(&BTreeSet::from([route]));
        }
        self.drop_outbound_member(|_, member| member.route == route)
    }

    /// What a member plays for one admitted session, resolved against the group as its leader
    /// writes it now.
    #[must_use]
    pub fn grant(
        &self,
        state: &SpeakerState,
        local: &DeviceId,
        session_id: [u8; 16],
        source: &DeviceId,
    ) -> Option<MemberPlaybackGrant> {
        let session = self.inbound.get(&session_id)?;
        if &session.source != source || !Self::consent_covers(state, local, session.group) {
            return None;
        }
        let group = state.group(session.group)?;
        let index = group
            .members
            .iter()
            .position(|member| &member.device == local)?;
        let member = group.members.get(index)?;
        Some(MemberPlaybackGrant {
            session_id,
            source: source.clone(),
            authorization: session.authorization,
            output: member.output,
            channel: *group.channel_roles().get(index)?,
            trim: member.trim.clone(),
        })
    }
}

fn build_group(
    id: SpeakerGroupId,
    name: &str,
    local: &DeviceId,
    members: &[MemberSpec],
    trims: &BTreeMap<(DeviceId, EndpointSelector, SpeakerRole), SpeakerTrim>,
) -> Result<SpeakerGroup, String> {
    let name = name.trim();
    let group = SpeakerGroup {
        id,
        name: name.to_string(),
        leader: local.clone(),
        members: members
            .iter()
            .map(|spec| SpeakerGroupMember {
                device: spec.device.clone(),
                output: spec.output,
                role: spec.role,
                trim: trims
                    .get(&(spec.device.clone(), spec.output, spec.role))
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect(),
        stamp: FabricStamp {
            counter: 0,
            device: local.clone(),
        },
    };
    if !tilo_network_actor::fabric::speaker_group::valid_group_name(&group.name) {
        return Err(crate::i18n::t("speaker-error-name"));
    }
    group
        .validate(local)
        .map_err(|_| crate::i18n::t("speaker-error-roles"))?;
    Ok(group)
}

fn removed_members(previous: &SpeakerGroup, next: &SpeakerGroup) -> BTreeSet<DeviceId> {
    previous
        .members
        .iter()
        .filter(|member| member.device != previous.leader && next.member(&member.device).is_none())
        .map(|member| member.device.clone())
        .collect()
}

/// The route id one member's audio travels under within `session_id`.
#[must_use]
pub fn member_route_id(
    session_id: [u8; 16],
    device: &DeviceId,
    output: EndpointSelector,
) -> [u8; 16] {
    let mut material = Vec::with_capacity(48);
    material.extend_from_slice(&session_id);
    material.extend_from_slice(&output.0);
    material.extend_from_slice(device.as_str().as_bytes());
    let digest = blake3::derive_key("so.tilo.speaker-group.member-route.v1", &material);
    let mut route = [0; 16];
    route.copy_from_slice(&digest[..16]);
    if route == [0; 16] {
        route[0] = 1;
    }
    route
}

fn stop_members(session: &GroupSession) -> Vec<SpeakerWrite> {
    session
        .members
        .iter()
        .filter(|member| {
            matches!(
                member.phase,
                SessionMemberPhase::AwaitingAdmission | SessionMemberPhase::Admitted
            )
        })
        .map(|member| SpeakerWrite::Stop {
            route: member.route,
            authorization: member.authorization,
        })
        .collect()
}

fn consent_matches(
    state: &SpeakerState,
    local: &DeviceId,
    group: SpeakerGroupId,
    pending: &PendingConsent,
) -> bool {
    state.group(group).is_some_and(|published| {
        published.leader == pending.leader
            && published.member(local).is_some_and(|member| {
                member.output == pending.output && member.role == pending.role
            })
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerMemberView {
    pub device: DeviceId,
    pub device_label: String,
    pub output: EndpointSelector,
    pub output_label: Option<String>,
    pub role: SpeakerRole,
    pub trim: SpeakerTrim,
    pub state: SpeakerMemberStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerGroupView {
    pub id: SpeakerGroupId,
    pub name: String,
    pub leader: DeviceId,
    pub leader_label: String,
    pub led_locally: bool,
    pub members: Vec<SpeakerMemberView>,
}

impl SpeakerGroupView {
    /// Members that can play now.
    #[must_use]
    pub fn ready_members(&self) -> usize {
        self.members
            .iter()
            .filter(|member| member.state == SpeakerMemberStatus::Ready)
            .count()
    }
}

/// Which of the two consents a group's leader asks a device for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpeakerConsentKind {
    Play,
    /// Listen with one of this device's microphones while calibrating.
    Listen,
}

/// What one consent lets the group's leader do on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakerConsentScope {
    Play(SpeakerRole),
    Listen,
}

impl SpeakerConsentScope {
    #[must_use]
    pub const fn kind(self) -> SpeakerConsentKind {
        match self {
            Self::Play(_) => SpeakerConsentKind::Play,
            Self::Listen => SpeakerConsentKind::Listen,
        }
    }
}

/// "Join 'Living room' as the right speaker?" or "Let the office use this microphone to
/// calibrate 'Living room'?" on the asked device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerConsentView {
    pub group: SpeakerGroupId,
    pub name: String,
    pub leader: DeviceId,
    pub leader_label: String,
    pub scope: SpeakerConsentScope,
    /// The output it would play on, or the microphone it would listen with.
    pub endpoint_label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerMembershipView {
    pub group: SpeakerGroupId,
    pub name: String,
    pub leader_label: String,
    pub scope: SpeakerConsentScope,
}

impl SpeakerConsentView {
    fn args(&self) -> Vec<(&'static str, ui::i18n::ArgValue)> {
        let mut args = vec![
            ("group", self.name.as_str().into()),
            ("leader", self.leader_label.as_str().into()),
        ];
        if let SpeakerConsentScope::Play(role) = self.scope {
            args.push(("role", role.describe().into()));
        }
        args
    }

    const fn key(&self, play: &'static str, listen: &'static str) -> &'static str {
        match self.scope {
            SpeakerConsentScope::Play(_) => play,
            SpeakerConsentScope::Listen => listen,
        }
    }

    /// The question, as the Devices surface asks it.
    #[must_use]
    pub fn prompt(&self) -> String {
        crate::i18n::t_args(
            self.key("qs-devices-speaker-join", "qs-devices-speaker-listen"),
            &self.args(),
        )
    }

    /// Who asked, and what exactly it would use.
    #[must_use]
    pub fn note(&self) -> String {
        let mut args = self.args();
        let key = match (&self.scope, &self.endpoint_label) {
            (SpeakerConsentScope::Play(_), None) => "qs-devices-speaker-join-note",
            (SpeakerConsentScope::Play(_), Some(label)) => {
                args.push(("output", label.as_str().into()));
                "qs-devices-speaker-join-note-output"
            }
            (SpeakerConsentScope::Listen, None) => "qs-devices-speaker-listen-note",
            (SpeakerConsentScope::Listen, Some(label)) => {
                args.push(("microphone", label.as_str().into()));
                "qs-devices-speaker-listen-note-microphone"
            }
        };
        crate::i18n::t_args(key, &args)
    }

    #[must_use]
    pub fn accessible_name(&self) -> String {
        crate::i18n::t_args(
            self.key("a11y-speaker-join", "a11y-speaker-listen"),
            &self.args(),
        )
    }

    #[must_use]
    pub fn accessible_description(&self) -> String {
        crate::i18n::t_args(
            self.key(
                "a11y-speaker-join-description",
                "a11y-speaker-listen-description",
            ),
            &self.args(),
        )
    }

    #[must_use]
    pub fn allow_label(&self) -> String {
        crate::i18n::t(self.key("a11y-speaker-join-allow", "a11y-speaker-listen-allow"))
    }

    #[must_use]
    pub fn decline_label(&self) -> String {
        crate::i18n::t(self.key("a11y-speaker-join-decline", "a11y-speaker-listen-decline"))
    }

    /// Title of the allow or decline action search offers.
    #[must_use]
    pub fn action_title(&self, approved: bool) -> String {
        let key = match (self.scope, approved) {
            (SpeakerConsentScope::Play(_), true) => "search-continuity-speaker-join",
            (SpeakerConsentScope::Play(_), false) => "search-continuity-speaker-decline",
            (SpeakerConsentScope::Listen, true) => "search-continuity-speaker-listen-allow",
            (SpeakerConsentScope::Listen, false) => "search-continuity-speaker-listen-decline",
        };
        crate::i18n::t_args(key, &self.args())
    }

    #[must_use]
    pub fn action_subtitle(&self) -> String {
        crate::i18n::t_args(
            self.key(
                "search-continuity-speaker-consent-subtitle",
                "search-continuity-speaker-listen-subtitle",
            ),
            &[("device", self.leader_label.as_str().into())],
        )
    }
}

impl SpeakerMembershipView {
    const fn key(&self, play: &'static str, listen: &'static str) -> &'static str {
        match self.scope {
            SpeakerConsentScope::Play(_) => play,
            SpeakerConsentScope::Listen => listen,
        }
    }

    /// The catalogue key naming what selecting this membership's row does.
    #[must_use]
    pub const fn action_key(&self) -> &'static str {
        self.key("qs-devices-action-leave", "qs-devices-action-stop")
    }

    fn args(&self, sentence: impl Fn(&str) -> String) -> Vec<(&'static str, ui::i18n::ArgValue)> {
        let mut args = vec![
            ("group", self.name.as_str().into()),
            ("leader", self.leader_label.as_str().into()),
        ];
        if let SpeakerConsentScope::Play(role) = self.scope {
            args.push(("role", sentence(&role.describe()).into()));
        }
        args
    }

    /// The row's title; `sentence` capitalises a role that starts it.
    #[must_use]
    pub fn title(&self, sentence: impl Fn(&str) -> String) -> String {
        crate::i18n::t_args(
            self.key("a11y-speaker-membership", "a11y-speaker-listener"),
            &self.args(sentence),
        )
    }

    #[must_use]
    pub fn note(&self) -> String {
        crate::i18n::t_args(
            self.key(
                "qs-devices-speaker-membership-note",
                "qs-devices-speaker-listener-note",
            ),
            &self.args(str::to_string),
        )
    }

    #[must_use]
    pub fn accessible_description(&self) -> String {
        crate::i18n::t(self.key(
            "a11y-speaker-membership-description",
            "a11y-speaker-listener-description",
        ))
    }

    #[must_use]
    pub fn leave_title(&self) -> String {
        crate::i18n::t_args(
            self.key(
                "search-continuity-speaker-leave",
                "search-continuity-speaker-listen-stop",
            ),
            &self.args(str::to_string),
        )
    }

    #[must_use]
    pub fn leave_subtitle(&self) -> String {
        crate::i18n::t_args(
            self.key(
                "search-continuity-speaker-leave-subtitle",
                "search-continuity-speaker-listen-stop-subtitle",
            ),
            &self.args(str::to_string),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerSessionView {
    pub session_id: [u8; 16],
    pub group: SpeakerGroupId,
    pub name: String,
    /// This device is a member playing for another device.
    pub inbound: bool,
    pub peer_label: Option<String>,
    /// Members playing in time.
    pub playing_members: usize,
    /// Members that have not admitted the session yet.
    pub waiting_members: usize,
    /// Admitted members that do not have the playing device's clock yet:
    /// they play nothing until they do.
    pub syncing_members: usize,
    /// Admitted members that could not get the clock within its deadline.
    pub failed_members: usize,
    /// Why, for the first of them.
    pub clock_failure: Option<ClockSyncFailure>,
}

impl SpeakerSessionView {
    /// What a person should read about this session first.
    #[must_use]
    pub const fn status(&self) -> SpeakerSessionStatus {
        if let Some(reason) = self.clock_failure {
            SpeakerSessionStatus::ClockFailed(reason)
        } else if self.syncing_members > 0 {
            SpeakerSessionStatus::SyncingClocks
        } else if self.waiting_members > 0 {
            SpeakerSessionStatus::Connecting
        } else {
            SpeakerSessionStatus::Playing
        }
    }
}

/// The one line a session is summarised by, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakerSessionStatus {
    /// A member cannot play in time, and why.
    ClockFailed(ClockSyncFailure),
    /// A member is still learning the playing device's clock.
    SyncingClocks,
    /// A member has not admitted the session yet.
    Connecting,
    Playing,
}

/// Tally of members by whether they can play in time.
#[derive(Default)]
struct SessionClocks {
    ready: usize,
    syncing: usize,
    failed: usize,
    failure: Option<ClockSyncFailure>,
}

impl SessionClocks {
    fn count(&mut self, state: ClockSyncState) {
        match state {
            ClockSyncState::Ready => self.ready = self.ready.saturating_add(1),
            ClockSyncState::Syncing => self.syncing = self.syncing.saturating_add(1),
            ClockSyncState::Failed(reason) => {
                self.failed = self.failed.saturating_add(1);
                self.failure.get_or_insert(reason);
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpeakerGroupsView {
    pub groups: Vec<SpeakerGroupView>,
    pub consents: Vec<SpeakerConsentView>,
    pub memberships: Vec<SpeakerMembershipView>,
    pub sessions: Vec<SpeakerSessionView>,
}

/// One audio route this device runs for a speaker group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteSpec {
    /// Send `source` to a member that admitted it.
    Send {
        route: [u8; 16],
        target: DeviceId,
        authorization: [u8; 16],
        source: GroupAudioSource,
    },
    /// Play `source` on one of this device's own outputs, as a member.
    Local {
        route: [u8; 16],
        source: GroupAudioSource,
        output: EndpointSelector,
        channel: media::sync_playout::ChannelRole,
        trim: SpeakerTrim,
    },
    /// Play what `source` sends, as this device's role in the group.
    Receive {
        route: [u8; 16],
        source: DeviceId,
        authorization: [u8; 16],
        output: EndpointSelector,
        channel: media::sync_playout::ChannelRole,
        trim: SpeakerTrim,
    },
}

impl RouteSpec {
    #[must_use]
    pub const fn route(&self) -> [u8; 16] {
        match self {
            Self::Send { route, .. } | Self::Local { route, .. } | Self::Receive { route, .. } => {
                *route
            }
        }
    }
}

/// Which routes to end and which to start so the running set becomes `desired`. A route whose
/// parameters changed is restarted.
#[must_use]
pub fn route_changes(
    running: &BTreeMap<[u8; 16], RouteSpec>,
    desired: &[RouteSpec],
) -> (Vec<[u8; 16]>, Vec<RouteSpec>) {
    let wanted = desired
        .iter()
        .map(|spec| (spec.route(), spec))
        .collect::<BTreeMap<_, _>>();
    let end = running
        .iter()
        .filter(|(route, spec)| wanted.get(*route).is_none_or(|want| *want != *spec))
        .map(|(route, _)| *route)
        .collect::<Vec<_>>();
    let start = desired
        .iter()
        .filter(|spec| {
            running
                .get(&spec.route())
                .is_none_or(|current| current != *spec)
        })
        .cloned()
        .collect();
    (end, start)
}

/// One member as the playing device renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedMember {
    pub route: [u8; 16],
    pub device: DeviceId,
    pub output: EndpointSelector,
    pub channel: media::sync_playout::ChannelRole,
    pub trim: SpeakerTrim,
    /// The member-issued capability every frame must carry; `None` for this device's own output.
    pub authorization: Option<[u8; 16]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPlaybackPlan {
    pub session_id: [u8; 16],
    pub group: SpeakerGroupId,
    pub source: GroupAudioSource,
    pub members: Vec<PlannedMember>,
}

/// What this device plays as a member of one admitted session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberPlaybackGrant {
    pub session_id: [u8; 16],
    pub source: DeviceId,
    pub authorization: [u8; 16],
    pub output: EndpointSelector,
    pub channel: media::sync_playout::ChannelRole,
    pub trim: SpeakerTrim,
}

// ---------------------------------------------------------------------------
// Process-global runtime.

fn core() -> &'static Mutex<SpeakerGroupCore> {
    static CORE: OnceLock<Mutex<SpeakerGroupCore>> = OnceLock::new();
    CORE.get_or_init(|| Mutex::new(SpeakerGroupCore::default()))
}

fn unavailable() -> String {
    crate::i18n::t("speaker-error-unavailable")
}

/// The record of the person's devices as this device's daemon last read it, as speaker groups.
fn record_state(network: &dyn FabricLink) -> Option<SpeakerState> {
    network
        .person_devices()
        .map(|record| SpeakerState::of(&record))
}

/// The person's speaker groups as the record of their devices holds them, empty until this
/// device has read it.
#[must_use]
pub fn speaker_state(network: &dyn FabricLink) -> SpeakerState {
    record_state(network).unwrap_or_default()
}

fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX)
}

/// The typed action that makes `write` to the record of the person's devices, or `None` when
/// what it carries into that record does not encode.
fn action_of(write: SpeakerWrite) -> Option<Invoke<Devices>> {
    use tilo_workspace_service::workspaces::person_workspace::devices::Devices as Record;
    Some(match write {
        SpeakerWrite::Endpoints { device, endpoints } => endpoints_action(&device, &endpoints)?,
        SpeakerWrite::Lead { device, group } => lead_action(&device, group)?,
        SpeakerWrite::Drop { device, group } => {
            Record::DROP_SPEAKER_GROUP.with((device.as_str().to_owned(), group.to_hex()))
        }
        SpeakerWrite::Consent {
            device,
            consent,
            approved,
        } => consent_action(&device, &consent, approved),
        SpeakerWrite::Withdraw {
            device,
            group,
            leader,
            play,
        } => Record::WITHDRAW_SPEAKER_GROUP.with((
            device.as_str().to_owned(),
            group.to_hex(),
            leader.as_str().to_owned(),
            play,
        )),
        SpeakerWrite::Offer {
            route,
            session,
            source,
            target,
            group,
        } => Record::OFFER_SPEAKER_PLAYBACK.with((
            hex::encode(route),
            hex::encode(session),
            source.as_str().to_owned(),
            target.as_str().to_owned(),
            group.to_hex(),
            now_ms(),
        )),
        SpeakerWrite::Admit {
            route,
            authorization,
        } => match authorization {
            Some(authorization) => Record::ADMIT_SPEAKER_PLAYBACK.with((
                hex::encode(route),
                hex::encode(authorization),
                now_ms(),
            )),
            None => Record::REFUSE_SPEAKER_PLAYBACK.with((hex::encode(route), now_ms())),
        },
        SpeakerWrite::Stop {
            route,
            authorization,
        } => Record::STOP_SPEAKER_PLAYBACK.with((
            hex::encode(route),
            authorization.map(hex::encode).unwrap_or_default(),
        )),
    })
}

/// The action recording the outputs and microphones this device offers.
fn endpoints_action(device: &DeviceId, endpoints: &[AudioEndpoint]) -> Option<Invoke<Devices>> {
    let endpoints = endpoints
        .iter()
        .map(|endpoint| record::SpeakerEndpoint {
            selector: endpoint.selector.to_hex(),
            microphone: endpoint.kind == AudioEndpointKind::Microphone,
            label: endpoint.label.clone(),
            is_default: endpoint.is_default,
        })
        .collect::<Vec<_>>();
    let bytes = postcard::to_allocvec(&endpoints).ok()?;
    Some(Devices::SAY_SPEAKER_ENDPOINTS.with((device.as_str().to_owned(), hex::encode(bytes))))
}

/// The action publishing a group this device leads, with its members and their trims.
fn lead_action(device: &DeviceId, group: SpeakerGroup) -> Option<Invoke<Devices>> {
    let members = group
        .members
        .iter()
        .map(|member| {
            let (delay_ns, gain_mb, eq) = trim_to_record(&member.trim);
            record::SpeakerLedMember {
                device: member.device.as_str().to_owned(),
                output: member.output.to_hex(),
                role: role_to_record(member.role),
                delay_ns,
                gain_mb,
                eq,
            }
        })
        .collect::<Vec<_>>();
    let bytes = postcard::to_allocvec(&members).ok()?;
    Some(Devices::LEAD_SPEAKER_GROUP.with((
        device.as_str().to_owned(),
        group.id.to_hex(),
        group.name,
        hex::encode(bytes),
        now_ms(),
    )))
}

/// The action recording this device's answer to a consent prompt.
fn consent_action(device: &DeviceId, consent: &ConsentWrite, approved: bool) -> Invoke<Devices> {
    use tilo_workspace_service::workspaces::person_workspace::devices::Devices as Record;
    let (group, leader, play, output, role, microphone) = match consent {
        ConsentWrite::Play {
            group,
            leader,
            output,
            role,
        } => (
            group.to_hex(),
            leader.as_str().to_owned(),
            true,
            output.to_hex(),
            role_to_record(*role),
            String::new(),
        ),
        ConsentWrite::Listen {
            group,
            leader,
            microphone,
        } => (
            group.to_hex(),
            leader.as_str().to_owned(),
            false,
            String::new(),
            String::new(),
            microphone.to_hex(),
        ),
    };
    let endpoint = if play { output } else { microphone };
    Record::CONSENT_SPEAKER_GROUP.with(
        tilo_workspace_service::workspaces::person_workspace::devices::SpeakerConsentGiven {
            device: device.as_str().to_owned(),
            group,
            leader,
            play,
            endpoint,
            role,
            approved,
            at_ms: now_ms(),
        },
    )
}

/// Make every `write` to the record of the person's devices, telling what the record refused.
fn apply_writes(network: &dyn FabricLink, writes: Vec<SpeakerWrite>) {
    for write in writes {
        let Some(action) = action_of(write) else {
            tracing::warn!("a speaker group change does not encode");
            continue;
        };
        if let Err(error) = network.apply_person_devices(action) {
            tracing::warn!(%error, "a speaker group change did not reach the person's record");
        }
    }
}

fn local_device(network: &dyn FabricLink) -> Result<DeviceId, String> {
    network
        .local_device_fabric_identity()
        .map(|(device, _)| device)
        .ok_or_else(|| crate::i18n::t("device-fabric-error-no-identity"))
}

/// Handle one authenticated live action. Returns true when it was a speaker-group action,
/// including a rejected one.
pub fn handle_action(
    network: &dyn FabricLink,
    envelope: &tilo_network_actor::fabric::PersonalFabricActionEnvelope,
) -> bool {
    let Ok(local) = local_device(network) else {
        return false;
    };
    if let PersonalFabricAction::SpeakerIdentify { output } = envelope.action {
        let _ = play_identify_locally(&local, output);
        return true;
    }
    let Some(state) = record_state(network) else {
        return false;
    };
    if let PersonalFabricAction::SpeakerSweep { group, start_ns } = envelope.action {
        let output = core()
            .lock()
            .ok()
            .and_then(|core| core.sweep_output(&state, &local, &envelope.source, group));
        if let Some(output) = output {
            play_sweep_at(&local, output, start_ns);
        }
        return true;
    }
    if let PersonalFabricAction::SpeakerListen {
        group,
        sweep,
        microphone,
        start_ns,
    } = envelope.action
    {
        let fabric = network.devices();
        let allowed = core().lock().is_ok_and(|core| {
            core.may_listen(&state, &fabric, &local, &envelope.source, group, microphone)
        });
        crate::calibration::listen_for_leader(
            &local,
            allowed,
            crate::calibration::ListenRequest {
                leader: envelope.source.clone(),
                group,
                sweep,
                microphone,
                start_ns,
            },
        );
        return true;
    }
    if let PersonalFabricAction::SpeakerHeard {
        group,
        sweep,
        hearing,
    } = &envelope.action
    {
        crate::calibration::deliver_hearing(&envelope.source, *group, *sweep, hearing.clone());
        return true;
    }
    let out = {
        let Ok(mut core) = core().lock() else {
            return false;
        };
        core.handle(
            &state,
            &local,
            &envelope.source,
            &envelope.action,
            Instant::now(),
        )
    };
    match out {
        Some((writes, live)) => {
            apply_writes(network, writes);
            for item in live {
                let _ = network.try_send_personal_fabric_action(&item.target, item.action);
            }
            true
        }
        None => false,
    }
}

/// Play the identify sound on `output` of `device`: locally, or by asking that device to play it
/// on its own output.
///
/// # Errors
/// Returns a message when the device or output is unavailable, a sound played too recently, or
/// the request cannot be delivered.
pub fn identify(
    network: &dyn FabricLink,
    device: &DeviceId,
    output: EndpointSelector,
) -> Result<(), String> {
    let local = local_device(network)?;
    if device == &local {
        return play_identify_locally(&local, output);
    }
    let state = record_state(network).unwrap_or_default();
    let offline = network
        .devices()
        .get(device)
        .is_none_or(|known| !known.online);
    if offline || state.output(device, output).is_none() {
        return Err(crate::i18n::t("speaker-error-speaker-unavailable"));
    }
    network
        .try_send_personal_fabric_action(device, PersonalFabricAction::SpeakerIdentify { output })
        .map_err(|error| error.to_string())
        .map(|_| ())
}

fn play_identify_locally(local: &DeviceId, output: EndpointSelector) -> Result<(), String> {
    let sink = resolve_local_output(local, output)
        .ok_or_else(|| crate::i18n::t("speaker-error-output-disconnected"))?;
    if !crate::settings::admit_identify(Instant::now()) {
        return Err(crate::i18n::t("speaker-error-already-playing"));
    }
    std::thread::Builder::new()
        .name("speaker-identify".into())
        .spawn(move || media::audio::group_tap::play_mono(&sink.name, &identify_chirp()))
        .map(|_| ())
        .map_err(|error| {
            crate::i18n::t_args(
                "speaker-error-play-failed",
                &[("error", error.to_string().into())],
            )
        })
}

/// Play the calibration sweep on `output` when this device's clock reads `start_ns`; the leader
/// already converted its schedule through its estimate of this clock. One sweep at a time;
/// requests too far from now are ignored, and the leader's measurement then reports no sweep.
fn play_sweep_at(local: &DeviceId, output: EndpointSelector, start_ns: i64) {
    static SWEEPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let lead = start_ns.saturating_sub(media::pw_stream::monotonic_now_ns());
    if !(0..=10_000_000_000).contains(&lead) {
        return;
    }
    let Some(sink) = resolve_local_output(local, output) else {
        return;
    };
    if SWEEPING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("speaker-sweep".into())
        .spawn(move || {
            if let Ok(sweep) = crate::calibration::sweep() {
                let _ = media::pw_stream::TimedAudio::native().play_at(
                    sink.serial,
                    crate::calibration::SWEEP_RATE,
                    1,
                    sweep.signal().to_vec(),
                    start_ns,
                );
            }
            SWEEPING.store(false, std::sync::atomic::Ordering::Release);
        });
    if spawned.is_err() {
        SWEEPING.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Two rising tones with soft edges: short, unmistakable, never loud.
#[must_use]
pub fn identify_chirp() -> Vec<i16> {
    const RATE: f64 = 48_000.0;
    const TONE: usize = 12_000;
    let as_f64 = |value: usize| f64::from(u32::try_from(value).unwrap_or(u32::MAX));
    let tone = move |frequency: f64| {
        (0..TONE).map(move |index| {
            let at = as_f64(index);
            let edge = (at.min(as_f64(TONE.saturating_sub(index))) / 1_200.0).min(1.0);
            let sample = (std::f64::consts::TAU * frequency * at / RATE).sin() * edge * 0.3;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the sample is bounded to a third of full scale"
            )]
            let value = (sample * f64::from(i16::MAX)) as i16;
            value
        })
    };
    tone(660.0).chain(tone(990.0)).collect()
}

/// Periodic maintenance from the fabric runtime's pump.
pub fn pump(network: &dyn FabricLink, media_enabled: bool) {
    let Ok(local) = local_device(network) else {
        return;
    };
    let Some(state) = record_state(network) else {
        return;
    };
    let fabric = network.devices();
    let mut writes = Vec::new();
    if media_enabled {
        let endpoints = local_endpoints(&local, Instant::now());
        // While PipeWire cannot be read there is nothing to write: what the device offered
        // before stays, rather than a failed read emptying its outputs from the record.
        if !endpoints.is_empty() && state.endpoints_of(&local) != endpoints.as_slice() {
            writes.push(SpeakerWrite::Endpoints {
                device: local.clone(),
                endpoints,
            });
        }
    }
    let out = core().lock().map_or_else(
        |_| Vec::new(),
        |mut core| {
            core.reconcile(
                &state,
                &fabric,
                &local,
                media_enabled,
                Instant::now(),
                &mut tilo_network_actor::fabric::nonzero::<16>,
            )
        },
    );
    writes.extend(out);
    apply_writes(network, writes);
    let answers = crate::calibration::drain_answers();
    if media_enabled {
        for (leader, action) in answers {
            let _ = network.try_send_personal_fabric_action(&leader, action);
        }
    }
    reconcile_routes(network, &state, &local, media_enabled);
    crate::settings::pump(network);
}

/// Routes this process started, routes starting on a worker, and routes whose start failed
/// recently.
#[derive(Default)]
struct RunningRoutes {
    running: BTreeMap<[u8; 16], RouteSpec>,
    starting: BTreeSet<[u8; 16]>,
    retry_after: BTreeMap<[u8; 16], Instant>,
}

const ROUTE_RETRY: Duration = Duration::from_secs(2);

fn running_routes() -> &'static Mutex<RunningRoutes> {
    static ROUTES: OnceLock<Mutex<RunningRoutes>> = OnceLock::new();
    ROUTES.get_or_init(|| Mutex::new(RunningRoutes::default()))
}

/// Bring the audio routes in line with the sessions: start routes for newly admitted members,
/// end routes whose session or consent ended, and end the session of a route the audio path
/// dropped on its own.
fn reconcile_routes(
    network: &dyn FabricLink,
    state: &SpeakerState,
    local: &DeviceId,
    media_enabled: bool,
) {
    let desired = if media_enabled {
        core()
            .lock()
            .map_or_else(|_| Vec::new(), |core| core.route_specs(state, local))
    } else {
        Vec::new()
    };
    let Ok(mut routes) = running_routes().lock() else {
        return;
    };
    let lost = routes
        .running
        .keys()
        .copied()
        .filter(|route| !crate::remote_audio::has_group_route(*route))
        .collect::<Vec<_>>();
    for route in &lost {
        routes.running.remove(route);
    }
    let (end, start) = route_changes(&routes.running, &desired);
    for route in end {
        routes.running.remove(&route);
        crate::remote_audio::end_group_route(route);
    }
    let now = Instant::now();
    routes.retry_after.retain(|_, until| *until > now);
    for spec in start {
        let route = spec.route();
        if routes.retry_after.contains_key(&route) || !routes.starting.insert(route) {
            continue;
        }
        // Opening sinks and streams talks to the audio daemon; never on the compositor thread.
        let local = local.clone();
        let spawned = std::thread::Builder::new()
            .name("speaker-route".into())
            .spawn(move || {
                let result = start_route(&local, &spec);
                let Ok(mut routes) = running_routes().lock() else {
                    return;
                };
                routes.starting.remove(&route);
                match result {
                    Ok(()) => {
                        routes.running.insert(route, spec);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not start a speaker group route");
                        routes.retry_after.insert(
                            route,
                            Instant::now()
                                .checked_add(ROUTE_RETRY)
                                .unwrap_or_else(Instant::now),
                        );
                    }
                }
            });
        if spawned.is_err() {
            routes.starting.remove(&route);
            routes
                .retry_after
                .insert(route, now.checked_add(ROUTE_RETRY).unwrap_or(now));
        }
    }
    drop(routes);
    if !lost.is_empty() {
        let out = core().lock().map_or_else(
            |_| Vec::new(),
            |mut core| {
                lost.iter()
                    .flat_map(|route| core.route_lost(*route))
                    .collect::<Vec<_>>()
            },
        );
        apply_writes(network, out);
    }
}

fn audio_source(source: &GroupAudioSource) -> crate::remote_audio::GroupRouteSource {
    match source {
        GroupAudioSource::PickerToken(token) => {
            crate::remote_audio::GroupRouteSource::Picker(*token)
        }
        // The media keeps decoding here, so everything this device plays is what the group
        // should hear.
        GroupAudioSource::MediaSession(_) | GroupAudioSource::System => {
            crate::remote_audio::GroupRouteSource::System
        }
    }
}

fn member_output(local: &DeviceId, output: EndpointSelector) -> Result<u32, String> {
    resolve_local_output(local, output)
        .map(|sink| sink.serial)
        .ok_or_else(|| crate::i18n::t("speaker-error-speaker-output-gone"))
}

fn start_route(local: &DeviceId, spec: &RouteSpec) -> Result<(), String> {
    use crate::remote_audio::{
        GroupPlayback, start_group_local, start_group_receive, start_group_send,
    };
    match spec {
        RouteSpec::Send {
            route,
            target,
            authorization,
            source,
        } => start_group_send(*route, target, *authorization, audio_source(source)),
        RouteSpec::Local {
            route,
            source,
            output,
            channel,
            trim,
        } => start_group_local(
            *route,
            audio_source(source),
            &GroupPlayback::new(member_output(local, *output)?, *channel, trim),
        ),
        RouteSpec::Receive {
            route,
            source,
            authorization,
            output,
            channel,
            trim,
        } => start_group_receive(
            *route,
            source,
            local,
            *authorization,
            &GroupPlayback::new(member_output(local, *output)?, *channel, trim),
        ),
    }
}

/// Whether this device admitted `route` from exactly `source`; the live audio path checks it
/// for every group frame.
#[must_use]
pub fn admits_route(route: [u8; 16], source: &DeviceId) -> bool {
    core().lock().is_ok_and(|core| core.admits(route, source))
}

fn with_core<T>(
    network: &dyn FabricLink,
    apply: impl FnOnce(
        &mut SpeakerGroupCore,
        &SpeakerState,
        &PersonDevices,
        &DeviceId,
    ) -> Result<(T, Vec<SpeakerWrite>), String>,
) -> Result<T, String> {
    let local = local_device(network)?;
    let state = record_state(network).ok_or_else(|| {
        "this device has not read the record of the person's devices yet".to_owned()
    })?;
    let fabric = network.devices();
    let mut core = core().lock().map_err(|_| unavailable())?;
    let applied = apply(&mut core, &state, &fabric, &local);
    drop(core);
    let (value, out) = applied?;
    apply_writes(network, out);
    Ok(value)
}

/// Create a group led by this device.
pub fn create_group(
    network: &dyn FabricLink,
    name: &str,
    members: &[MemberSpec],
) -> Result<SpeakerGroupId, String> {
    with_core(network, |core, state, fabric, local| {
        let id = SpeakerGroupId(tilo_network_actor::fabric::nonzero::<16>());
        let out = core.create_group(state, fabric, local, id, name, members, Instant::now())?;
        Ok((id, out))
    })
}

/// Rename or re-member a group led by this device.
pub fn update_group(
    network: &dyn FabricLink,
    id: SpeakerGroupId,
    name: &str,
    members: &[MemberSpec],
) -> Result<(), String> {
    with_core(network, |core, state, fabric, local| {
        let out = core.update_group(state, fabric, local, id, name, members, Instant::now())?;
        Ok(((), out))
    })
}

/// Store calibration trims on a group led by this device.
pub fn set_trims(
    network: &dyn FabricLink,
    id: SpeakerGroupId,
    trims: &[(DeviceId, EndpointSelector, SpeakerTrim)],
) -> Result<(), String> {
    with_core(network, |core, state, _, local| {
        let out = core.set_trims(state, local, id, trims)?;
        Ok(((), out))
    })
}

/// Delete a group led by this device.
pub fn delete_group(network: &dyn FabricLink, id: SpeakerGroupId) -> Result<(), String> {
    with_core(network, |core, _, _, local| {
        Ok(((), core.delete_group(local, id)))
    })
}

/// Answer a join or microphone prompt on this device.
pub fn resolve_consent(
    network: &dyn FabricLink,
    group: SpeakerGroupId,
    kind: SpeakerConsentKind,
    approved: bool,
) -> Result<(), String> {
    let state = record_state(network).unwrap_or_default();
    with_core(network, |core, _, _, local| {
        let out = match kind {
            SpeakerConsentKind::Play => {
                core.resolve_consent(&state, local, group, approved, Instant::now())?
            }
            SpeakerConsentKind::Listen => {
                core.resolve_listen_consent(&state, local, group, approved, Instant::now())?
            }
        };
        Ok(((), out))
    })
}

/// Leave a group led by another device, or stop letting it listen with this device's microphone.
pub fn leave_group(
    network: &dyn FabricLink,
    group: SpeakerGroupId,
    kind: SpeakerConsentKind,
) -> Result<(), String> {
    with_core(network, |core, state, _, local| {
        let out = match kind {
            SpeakerConsentKind::Play => core.leave(state, local, group)?,
            SpeakerConsentKind::Listen => core.stop_listening(state, local, group)?,
        };
        Ok(((), out))
    })
}

/// The person's microphones that could listen while calibrating a group this device leads.
#[must_use]
pub fn listener_candidates(
    network: &dyn FabricLink,
    group: &SpeakerGroup,
) -> Vec<ListenerCandidate> {
    let Ok(local) = local_device(network) else {
        return Vec::new();
    };
    let state = record_state(network).unwrap_or_default();
    let fabric = network.devices();
    core().lock().map_or_else(
        |_| Vec::new(),
        |core| core.listener_candidates(&state, &fabric, &local, group, Instant::now()),
    )
}

/// Ask the devices of `microphones` that have not answered yet to let this device listen while
/// calibrating `group`.
pub fn invite_listeners(
    network: &dyn FabricLink,
    group: &SpeakerGroup,
    microphones: &[(DeviceId, EndpointSelector)],
) -> Result<(), String> {
    let state = record_state(network).unwrap_or_default();
    let live = core().lock().map_err(|_| unavailable())?.invite_listeners(
        &state,
        group,
        microphones,
        Instant::now(),
    );
    for item in live {
        let _ = network.try_send_personal_fabric_action(&item.target, item.action);
    }
    Ok(())
}

/// Play `source` from this device to a group.
pub fn start_session(
    network: &dyn FabricLink,
    group: SpeakerGroupId,
    source: GroupAudioSource,
) -> Result<[u8; 16], String> {
    with_core(network, |core, state, fabric, local| {
        let session_id = tilo_network_actor::fabric::nonzero::<16>();
        let out = core.start_session(
            state,
            fabric,
            local,
            group,
            source,
            session_id,
            Instant::now(),
        )?;
        Ok((session_id, out))
    })
}

/// Stop one session in either direction.
pub fn stop_session(network: &dyn FabricLink, session_id: [u8; 16]) -> Result<(), String> {
    with_core(network, |core, _, _, _| {
        Ok(((), core.stop_session(session_id)?))
    })
}

#[must_use]
pub fn view(network: &dyn FabricLink) -> SpeakerGroupsView {
    let Ok(local) = local_device(network) else {
        return SpeakerGroupsView::default();
    };
    let state = record_state(network).unwrap_or_default();
    let fabric = network.devices();
    // Read before taking the core: the audio path takes the core while it holds its own state.
    let playouts = crate::remote_audio::group_route_playouts();
    core().lock().map_or_else(
        |_| SpeakerGroupsView::default(),
        |core| {
            core.view(&state, &fabric, &local, &|route| {
                playouts.get(&route).copied()
            })
        },
    )
}

/// Sessions this device is playing to groups, with each member it may send to. The timed audio
/// path renders exactly these.
#[must_use]
pub fn outbound_playback_plans() -> Vec<GroupPlaybackPlan> {
    core()
        .lock()
        .map_or_else(|_| Vec::new(), |core| core.playback_plans())
}

/// What this device plays for `session_id` from `source`, or `None` when it has not admitted
/// that exact session.
#[must_use]
pub fn member_grant(
    network: &dyn FabricLink,
    session_id: [u8; 16],
    source: &DeviceId,
) -> Option<MemberPlaybackGrant> {
    let local = local_device(network).ok()?;
    let state = record_state(network)?;
    core()
        .lock()
        .ok()?
        .grant(&state, &local, session_id, source)
}

/// Resolve one of this device's published selectors to its local output.
#[must_use]
pub fn resolve_local_output(
    local: &DeviceId,
    selector: EndpointSelector,
) -> Option<media::audio::AudioDevice> {
    media::audio::enumerate()
        .sinks
        .into_iter()
        .find(|sink| EndpointSelector::mint(local, &sink.name) == selector)
}

/// Resolve one of this device's published selectors to its local microphone.
#[must_use]
pub fn resolve_local_microphone(
    local: &DeviceId,
    selector: EndpointSelector,
) -> Option<media::audio::AudioDevice> {
    media::audio::enumerate()
        .sources
        .into_iter()
        .find(|source| EndpointSelector::mint(local, &source.name) == selector)
}

/// Build the published endpoint list from an enumerated audio graph.
#[must_use]
pub fn endpoints_from_graph(
    local: &DeviceId,
    graph: &media::audio::AudioGraph,
) -> Vec<AudioEndpoint> {
    let label = |device: &media::audio::AudioDevice| {
        let mut label = if device.description.is_empty() {
            device.name.clone()
        } else {
            device.description.clone()
        };
        label.retain(|character| !character.is_control());
        while label.len()
            > tilo_network_actor::fabric::speaker_group::MAX_AUDIO_ENDPOINT_LABEL_BYTES
        {
            label.pop();
        }
        label
    };
    let mut seen = BTreeSet::new();
    graph
        .sinks
        .iter()
        .map(|device| (device, AudioEndpointKind::Output))
        .chain(
            graph
                .sources
                .iter()
                .map(|device| (device, AudioEndpointKind::Microphone)),
        )
        .filter(|(device, _)| !device.name.is_empty())
        .map(|(device, kind)| AudioEndpoint {
            selector: EndpointSelector::mint(local, &device.name),
            kind,
            label: label(device),
            is_default: device.is_default,
        })
        .filter(|endpoint| !endpoint.label.is_empty() && seen.insert(endpoint.selector))
        .take(tilo_network_actor::fabric::speaker_group::MAX_AUDIO_ENDPOINTS)
        .collect()
}

#[cfg(test)]
mod tests;
