//! Consent, admission and teardown for speaker groups.
//!
//! [`SpeakerGroupCore`] is the whole policy, driven with explicit fabric
//! state and time so it is testable without a network. The process-global
//! wrapper below feeds it the live fabric and sends what it emits.
//!
//! Three things are kept apart:
//!
//! * **Configuration** — the groups this device leads. They roam in its
//!   snapshot and only this device edits them.
//! * **Consent** — memberships this device agreed to. Each is asked for once,
//!   on this device, and authored into this device's snapshot. A change to
//!   the member's output or role in the group no longer matches it, so the
//!   member is asked again.
//! * **Sessions** — one playback to a group. Any of the person's devices may
//!   start one; each member admits it only while its consent still covers
//!   the group as its leader publishes it, and answers with a fresh random
//!   capability bound to that exact source and session. Losing the source,
//!   the consent, the group, or media continuity ends the session.
//!
//! `PipeWire` identities stay inside [`resolve_local_output`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::shell::device_fabric::{DeviceFabric, FabricStamp, PersonalFabricAction};
use crate::shell::fabric_clock::{ClockSyncFailure, ClockSyncState};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerFabricState, SpeakerGroup,
    SpeakerGroupId, SpeakerGroupMember, SpeakerListener, SpeakerMemberStatus, SpeakerMembership,
    SpeakerRole, SpeakerTrim,
};

mod listening;
pub use listening::{ListenerCandidate, ListenerState};

/// How long an unanswered join prompt stays on the member device.
const CONSENT_TTL: Duration = Duration::from_secs(10 * 60);
/// How often the leader repeats an unanswered invite to an online member.
const INVITE_RETRY: Duration = Duration::from_secs(30);
/// How long a playing device waits for one member to admit a session.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on concurrent sessions in either direction.
pub const MAX_SPEAKER_SESSIONS: usize = 4;
/// How stale the published endpoint list may get before it is re-enumerated
/// off the calling thread.
const ENDPOINT_REFRESH: Duration = Duration::from_secs(5);

/// One fabric action to send to one exact device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Egress {
    pub target: DeviceId,
    pub action: PersonalFabricAction,
}

fn egress(target: &DeviceId, action: PersonalFabricAction) -> Egress {
    Egress {
        target: target.clone(),
        action,
    }
}

/// What a session plays. The playing device resolves it; it never crosses
/// the fabric.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupAudioSource {
    /// An entry of the shell's live-audio source picker.
    PickerToken(u64),
    /// A local media session, by its logical id.
    MediaSession(String),
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

#[derive(Clone, Debug)]
struct InviteRecord {
    output: EndpointSelector,
    role: SpeakerRole,
    sent: Instant,
    declined: bool,
}

/// The speaker-group policy, independent of transport and wall time.
#[derive(Default)]
pub struct SpeakerGroupCore {
    /// Groups this device leads. `None` until seeded from the persisted
    /// snapshot, so a restart keeps the configuration.
    groups: Option<Vec<SpeakerGroup>>,
    /// Consents this device gave, seeded the same way.
    memberships: Option<Vec<SpeakerMembership>>,
    invites: BTreeMap<(SpeakerGroupId, DeviceId), InviteRecord>,
    consents: BTreeMap<SpeakerGroupId, PendingConsent>,
    /// Microphone consents this device gave, seeded like memberships.
    listeners: Option<Vec<SpeakerListener>>,
    listen_invites: BTreeMap<(SpeakerGroupId, DeviceId), listening::ListenInvite>,
    listen_consents: BTreeMap<SpeakerGroupId, listening::PendingListen>,
    outbound: BTreeMap<[u8; 16], GroupSession>,
    inbound: BTreeMap<[u8; 16], MemberSession>,
    /// Media continuity was turned off; nothing starts until it is back.
    media_off: bool,
}

/// A request to edit one member of a group this device leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberSpec {
    pub device: DeviceId,
    pub output: EndpointSelector,
    pub role: SpeakerRole,
}

impl SpeakerGroupCore {
    /// Adopt the persisted records once, the first time local state is
    /// needed after start.
    pub fn seed(&mut self, previous: Option<&SpeakerFabricState>) {
        if self.groups.is_none() {
            self.groups = Some(previous.map_or_else(Vec::new, |state| state.groups.clone()));
        }
        if self.memberships.is_none() {
            self.memberships =
                Some(previous.map_or_else(Vec::new, |state| state.memberships.clone()));
        }
        if self.listeners.is_none() {
            self.listeners = Some(previous.map_or_else(Vec::new, |state| state.listeners.clone()));
        }
    }

    /// The records this device publishes, stamped for its next snapshot.
    #[must_use]
    pub fn authored(
        &self,
        endpoints: Vec<AudioEndpoint>,
        stamp: &FabricStamp,
    ) -> SpeakerFabricState {
        SpeakerFabricState {
            endpoints,
            groups: self
                .groups
                .iter()
                .flatten()
                .cloned()
                .map(|mut group| {
                    group.stamp = stamp.clone();
                    group
                })
                .collect(),
            memberships: self
                .memberships
                .iter()
                .flatten()
                .cloned()
                .map(|mut membership| {
                    membership.stamp = stamp.clone();
                    membership
                })
                .collect(),
            listeners: self
                .listeners
                .iter()
                .flatten()
                .cloned()
                .map(|mut listener| {
                    listener.stamp = stamp.clone();
                    listener
                })
                .collect(),
        }
    }

    fn groups_mut(&mut self) -> &mut Vec<SpeakerGroup> {
        self.groups.get_or_insert_with(Vec::new)
    }

    fn memberships_mut(&mut self) -> &mut Vec<SpeakerMembership> {
        self.memberships.get_or_insert_with(Vec::new)
    }

    #[must_use]
    pub fn led_group(&self, id: SpeakerGroupId) -> Option<&SpeakerGroup> {
        self.groups.iter().flatten().find(|group| group.id == id)
    }

    /// Create a group this device leads and invite its remote members.
    ///
    /// # Errors
    ///
    /// Returns a message when the name or member set is invalid, a member is
    /// not a certified device that can join, or too many groups exist.
    pub fn create_group(
        &mut self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        id: SpeakerGroupId,
        name: &str,
        members: &[MemberSpec],
        now: Instant,
    ) -> Result<Vec<Egress>, String> {
        if self.groups.iter().flatten().count() >= crate::shell::speaker_group::MAX_SPEAKER_GROUPS {
            return Err(crate::shell::i18n::t("speaker-error-too-many-groups"));
        }
        if self.led_group(id).is_some() {
            return Err(crate::shell::i18n::t("speaker-error-group-exists"));
        }
        let group = build_group(id, name, local, members, &BTreeMap::new())?;
        fabric
            .admit_speaker_group(&group)
            .map_err(|error| error.to_string())?;
        let invites = self.invite(&group, now, true);
        self.groups_mut().push(group);
        Ok(invites)
    }

    /// Replace the name and members of a group this device leads. Members
    /// keep their calibration while their output and role stay the same;
    /// removed members are told to leave, changed ones are asked again.
    ///
    /// # Errors
    ///
    /// Returns a message for an unknown group or an inadmissible member set.
    pub fn update_group(
        &mut self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        id: SpeakerGroupId,
        name: &str,
        members: &[MemberSpec],
        now: Instant,
    ) -> Result<Vec<Egress>, String> {
        let previous = self
            .led_group(id)
            .cloned()
            .ok_or_else(|| crate::shell::i18n::t("speaker-error-group-gone"))?;
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
        fabric
            .admit_speaker_group(&group)
            .map_err(|error| error.to_string())?;
        let mut out = removed_members(&previous, &group)
            .into_iter()
            .map(|device| {
                egress(
                    &device,
                    PersonalFabricAction::SpeakerGroupLeave { group: id },
                )
            })
            .collect::<Vec<_>>();
        self.invites
            .retain(|(group_id, device), _| *group_id != id || group.member(device).is_some());
        out.extend(self.invite(&group, now, true));
        out.extend(self.end_outbound_for_group(id));
        if let Some(slot) = self.groups_mut().iter_mut().find(|slot| slot.id == id) {
            *slot = group;
        }
        Ok(out)
    }

    /// Replace the calibration trims of a led group, member by member. The
    /// member set is unchanged, so no consent is asked again.
    ///
    /// # Errors
    ///
    /// Returns a message when the group is unknown, a trim names no member,
    /// or a trim is out of bounds.
    pub fn set_trims(
        &mut self,
        id: SpeakerGroupId,
        trims: &[(DeviceId, EndpointSelector, SpeakerTrim)],
    ) -> Result<(), String> {
        let group = self
            .groups_mut()
            .iter_mut()
            .find(|group| group.id == id)
            .ok_or_else(|| crate::shell::i18n::t("speaker-error-group-gone"))?;
        let mut updated = group.clone();
        for (device, output, trim) in trims {
            let member = updated
                .members
                .iter_mut()
                .find(|member| &member.device == device && member.output == *output)
                .ok_or_else(|| crate::shell::i18n::t("speaker-error-not-in-group"))?;
            member.trim = trim.clone();
        }
        updated
            .validate(&updated.leader.clone())
            .map_err(|_| crate::shell::i18n::t("speaker-error-calibration-range"))?;
        *group = updated;
        Ok(())
    }

    /// Delete a group this device leads and release every member.
    ///
    /// # Errors
    ///
    /// Returns a message for an unknown group.
    pub fn delete_group(&mut self, id: SpeakerGroupId) -> Result<Vec<Egress>, String> {
        let groups = self.groups_mut();
        let index = groups
            .iter()
            .position(|group| group.id == id)
            .ok_or_else(|| crate::shell::i18n::t("speaker-error-group-gone"))?;
        let group = groups.remove(index);
        self.invites.retain(|(group_id, _), _| *group_id != id);
        let listening = self
            .listen_invites
            .keys()
            .filter(|(group_id, device)| *group_id == id && group.member(device).is_none())
            .map(|(_, device)| device.clone())
            .collect::<BTreeSet<_>>();
        self.listen_invites
            .retain(|(group_id, _), _| *group_id != id);
        let mut out = group
            .members
            .iter()
            .filter(|member| member.device != group.leader)
            .map(|member| member.device.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .chain(listening)
            .map(|device| {
                egress(
                    &device,
                    PersonalFabricAction::SpeakerGroupLeave { group: id },
                )
            })
            .collect::<Vec<_>>();
        out.extend(self.end_outbound_for_group(id));
        Ok(out)
    }

    /// Invites for remote members that have not consented. `force` resends
    /// regardless of the retry interval and forgets earlier declines, for an
    /// explicit edit.
    fn invite(&mut self, group: &SpeakerGroup, now: Instant, force: bool) -> Vec<Egress> {
        group
            .members
            .iter()
            .filter(|member| member.device != group.leader)
            .filter_map(|member| {
                let key = (group.id, member.device.clone());
                let due = force
                    || self.invites.get(&key).is_none_or(|record| {
                        !record.declined
                            && (record.output != member.output
                                || record.role != member.role
                                || now.saturating_duration_since(record.sent) >= INVITE_RETRY)
                    });
                if !due {
                    return None;
                }
                self.invites.insert(
                    key,
                    InviteRecord {
                        output: member.output,
                        role: member.role,
                        sent: now,
                        declined: false,
                    },
                );
                Some(egress(
                    &member.device,
                    PersonalFabricAction::SpeakerGroupInvite {
                        group: group.id,
                        name: group.name.clone(),
                        output: member.output,
                        role: member.role,
                    },
                ))
            })
            .collect()
    }

    /// Resolve a join prompt on this device.
    ///
    /// # Errors
    ///
    /// Returns a message when the prompt expired or no longer matches the
    /// group as its leader publishes it.
    pub fn resolve_consent(
        &mut self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        group_id: SpeakerGroupId,
        approved: bool,
        stamp: &FabricStamp,
    ) -> Result<Vec<Egress>, String> {
        let pending = self
            .consents
            .remove(&group_id)
            .ok_or_else(|| crate::shell::i18n::t("speaker-error-request-expired"))?;
        if approved {
            if !consent_matches(fabric, local, group_id, &pending) {
                return Err(crate::shell::i18n::t("speaker-error-group-changed"));
            }
            if self.memberships.iter().flatten().count()
                >= crate::shell::speaker_group::MAX_SPEAKER_MEMBERSHIPS
            {
                self.consents.insert(group_id, pending);
                return Err(crate::shell::i18n::t("speaker-error-too-many-memberships"));
            }
            let memberships = self.memberships_mut();
            memberships.retain(|membership| membership.group != group_id);
            memberships.push(SpeakerMembership {
                group: group_id,
                leader: pending.leader.clone(),
                output: pending.output,
                role: pending.role,
                stamp: stamp.clone(),
            });
        }
        Ok(vec![egress(
            &pending.leader,
            PersonalFabricAction::SpeakerGroupDecision {
                group: group_id,
                accepted: approved,
            },
        )])
    }

    /// Withdraw this device's consent to a group led elsewhere.
    ///
    /// # Errors
    ///
    /// Returns a message when this device is not in that group.
    pub fn leave(&mut self, group_id: SpeakerGroupId) -> Result<Vec<Egress>, String> {
        let memberships = self.memberships_mut();
        let index = memberships
            .iter()
            .position(|membership| membership.group == group_id)
            .ok_or_else(|| crate::shell::i18n::t("speaker-error-not-a-member"))?;
        let membership = memberships.remove(index);
        let mut out = vec![egress(
            &membership.leader,
            PersonalFabricAction::SpeakerGroupLeave { group: group_id },
        )];
        out.extend(self.end_inbound(|session| session.group == group_id));
        Ok(out)
    }

    /// Start playing `source` to a group from this device.
    ///
    /// # Errors
    ///
    /// Returns a message when the group is unknown, no member is ready, or
    /// too many sessions are active.
    pub fn start_session(
        &mut self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        group_id: SpeakerGroupId,
        source: GroupAudioSource,
        session_id: [u8; 16],
        now: Instant,
    ) -> Result<Vec<Egress>, String> {
        if self.media_off {
            return Err(crate::shell::i18n::t("speaker-error-media-off"));
        }
        if self.outbound.len().saturating_add(self.inbound.len()) >= MAX_SPEAKER_SESSIONS {
            return Err(crate::shell::i18n::t("speaker-error-too-many-sessions"));
        }
        let group = fabric
            .speaker_group(group_id)
            .ok_or_else(|| crate::shell::i18n::t("speaker-error-group-unavailable"))?;
        let states = fabric.speaker_member_states(group);
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
            return Err(crate::shell::i18n::t("speaker-error-none-ready"));
        }
        let offers = members
            .iter()
            .filter(|member| member.phase == SessionMemberPhase::AwaitingAdmission)
            .map(|member| {
                egress(
                    &member.device,
                    PersonalFabricAction::SpeakerGroupSessionOffer {
                        session_id: member.route,
                        group: group_id,
                    },
                )
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
    ///
    /// # Errors
    ///
    /// Returns a message when the session is no longer active.
    pub fn stop_session(&mut self, session_id: [u8; 16]) -> Result<Vec<Egress>, String> {
        if let Some(session) = self.outbound.remove(&session_id) {
            return Ok(stop_members(&session));
        }
        if let Some(session) = self.inbound.remove(&session_id) {
            return Ok(vec![egress(
                &session.source,
                PersonalFabricAction::SpeakerGroupSessionStop {
                    session_id,
                    authorization: Some(session.authorization),
                },
            )]);
        }
        Err(crate::shell::i18n::t("speaker-error-session-gone"))
    }

    /// Consume one authenticated speaker-group action. Returns `None` for
    /// any other action.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive handler keeps each speaker action beside its admission rule"
    )]
    pub fn handle(
        &mut self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        source: &DeviceId,
        action: &PersonalFabricAction,
        now: Instant,
        mint: &mut dyn FnMut() -> [u8; 16],
    ) -> Option<Vec<Egress>> {
        if let Some(out) = self.handle_listen_action(source, action, now) {
            return Some(out);
        }
        let mut out = Vec::new();
        match action {
            PersonalFabricAction::SpeakerGroupInvite {
                group,
                output,
                role,
                ..
            } => {
                let already = self.memberships.iter().flatten().any(|membership| {
                    membership.group == *group
                        && &membership.leader == source
                        && membership.output == *output
                        && membership.role == *role
                });
                if already {
                    out.push(egress(
                        source,
                        PersonalFabricAction::SpeakerGroupDecision {
                            group: *group,
                            accepted: true,
                        },
                    ));
                } else if self.consents.len() < crate::shell::speaker_group::MAX_SPEAKER_MEMBERSHIPS
                    || self.consents.contains_key(group)
                {
                    self.consents.insert(
                        *group,
                        PendingConsent {
                            leader: source.clone(),
                            output: *output,
                            role: *role,
                            received: now,
                        },
                    );
                }
            }
            PersonalFabricAction::SpeakerGroupDecision { group, accepted } => {
                if let Some(record) = self.invites.get_mut(&(*group, source.clone())) {
                    record.declined = !*accepted;
                    record.sent = now;
                }
            }
            PersonalFabricAction::SpeakerGroupLeave { group } => {
                self.forget_listening(*group, source);
                let withdrawn = self
                    .led_group(*group)
                    .and_then(|led| led.member(source))
                    .map(|member| (member.output, member.role));
                if let Some((output, role)) = withdrawn {
                    // The member withdrew: stop asking until the group is
                    // edited, and drop it from sessions playing from here.
                    self.invites.insert(
                        (*group, source.clone()),
                        InviteRecord {
                            output,
                            role,
                            sent: now,
                            declined: true,
                        },
                    );
                    out.extend(self.drop_outbound_member(|session, member| {
                        session.group == *group && &member.device == source
                    }));
                }
                let removed = {
                    let memberships = self.memberships_mut();
                    let before = memberships.len();
                    memberships.retain(|membership| {
                        membership.group != *group || &membership.leader != source
                    });
                    before != memberships.len()
                };
                if self
                    .consents
                    .get(group)
                    .is_some_and(|pending| &pending.leader == source)
                {
                    self.consents.remove(group);
                }
                if removed {
                    out.extend(self.end_inbound(|session| session.group == *group));
                }
            }
            PersonalFabricAction::SpeakerGroupSessionOffer { session_id, group } => {
                let admitted = self.outbound.len().saturating_add(self.inbound.len())
                    < MAX_SPEAKER_SESSIONS
                    && !self.inbound.contains_key(session_id)
                    && self.consent_covers(fabric, local, *group);
                let authorization = admitted.then(&mut *mint);
                if let Some(authorization) = authorization {
                    self.inbound.insert(
                        *session_id,
                        MemberSession {
                            source: source.clone(),
                            group: *group,
                            authorization,
                        },
                    );
                }
                out.push(egress(
                    source,
                    PersonalFabricAction::SpeakerGroupSessionDecision {
                        session_id: *session_id,
                        group: *group,
                        authorization,
                    },
                ));
            }
            PersonalFabricAction::SpeakerGroupSessionDecision {
                session_id,
                group,
                authorization,
            } => {
                let mut unmatched = authorization.is_some();
                for session in self
                    .outbound
                    .values_mut()
                    .filter(|session| session.group == *group)
                {
                    for member in session.members.iter_mut().filter(|member| {
                        member.route == *session_id
                            && &member.device == source
                            && member.phase == SessionMemberPhase::AwaitingAdmission
                    }) {
                        unmatched = false;
                        member.authorization = *authorization;
                        member.phase = if authorization.is_some() {
                            SessionMemberPhase::Admitted
                        } else {
                            SessionMemberPhase::Refused
                        };
                    }
                }
                // A late admission for a session that already ended must not
                // leave a capability alive on the member.
                if unmatched {
                    out.push(egress(
                        source,
                        PersonalFabricAction::SpeakerGroupSessionStop {
                            session_id: *session_id,
                            authorization: *authorization,
                        },
                    ));
                }
            }
            PersonalFabricAction::SpeakerGroupSessionStop {
                session_id,
                authorization,
            } => {
                let inbound = self.inbound.get(session_id).is_some_and(|session| {
                    &session.source == source
                        && authorization.is_none_or(|value| value == session.authorization)
                });
                if inbound {
                    self.inbound.remove(session_id);
                }
                for member in self
                    .outbound
                    .values_mut()
                    .flat_map(|session| session.members.iter_mut())
                    .filter(|member| member.route == *session_id && &member.device == source)
                {
                    member.phase = SessionMemberPhase::Refused;
                    member.authorization = None;
                }
            }
            _ => return None,
        }
        Some(out)
    }

    /// Whether this device may admit a session for `group` right now: the
    /// leader publishes the group with this device as a member, and either
    /// this device is that leader or its consent covers the member as
    /// published.
    fn consent_covers(
        &self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        group: SpeakerGroupId,
    ) -> bool {
        let Some(published) = fabric.speaker_group(group) else {
            return false;
        };
        let Some(member) = published.member(local) else {
            return false;
        };
        &published.leader == local
            || self
                .memberships
                .iter()
                .flatten()
                .any(|membership| membership.covers(published, member))
    }

    /// Periodic maintenance. With media continuity disabled everything ends
    /// and consents are withdrawn; otherwise stale consent, sessions whose
    /// peers or consent vanished, and unanswered invites are handled.
    #[expect(
        clippy::too_many_lines,
        reason = "one maintenance pass keeps every teardown rule in policy order"
    )]
    pub fn pump(
        &mut self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        enabled: bool,
        now: Instant,
    ) -> Vec<Egress> {
        let mut out = self.pump_listening(fabric, local, enabled, now);
        self.media_off = !enabled;
        if !enabled {
            out.extend(self.end_inbound(|_| true));
            for session in std::mem::take(&mut self.outbound).into_values() {
                out.extend(stop_members(&session));
            }
            self.consents.clear();
            self.invites.clear();
            if let Some(memberships) = self.memberships.as_mut() {
                for membership in memberships.drain(..) {
                    out.push(egress(
                        &membership.leader,
                        PersonalFabricAction::SpeakerGroupLeave {
                            group: membership.group,
                        },
                    ));
                }
            }
            self.groups = None;
            return out;
        }

        // A consent whose group its leader no longer publishes as it was
        // agreed is void. Only a leader whose snapshot is known can void it.
        let stale = self
            .memberships
            .iter()
            .flatten()
            .filter(|membership| {
                fabric.snapshots().contains_key(&membership.leader)
                    && !fabric.speaker_group(membership.group).is_some_and(|group| {
                        group
                            .member(local)
                            .is_some_and(|member| membership.covers(group, member))
                    })
            })
            .map(|membership| membership.group)
            .collect::<BTreeSet<_>>();
        if !stale.is_empty() {
            self.memberships_mut()
                .retain(|membership| !stale.contains(&membership.group));
        }

        let online = |device: &DeviceId| {
            fabric
                .snapshots()
                .get(device)
                .is_some_and(|snapshot| snapshot.presence.online)
        };
        self.consents.retain(|group, pending| {
            now.saturating_duration_since(pending.received) < CONSENT_TTL
                && online(&pending.leader)
                && !(fabric.snapshots().contains_key(&pending.leader)
                    && fabric.speaker_group(*group).is_none())
        });

        let revoked = self
            .inbound
            .iter()
            .filter(|(_, session)| {
                !online(&session.source) || !self.consent_covers(fabric, local, session.group)
            })
            .map(|(session_id, _)| *session_id)
            .collect::<BTreeSet<_>>();
        out.extend(self.end_inbound_ids(&revoked));

        out.extend(self.drop_outbound_member(|session, member| {
            member.phase != SessionMemberPhase::Local
                && (!online(&member.device)
                    || (member.phase == SessionMemberPhase::AwaitingAdmission
                        && now.saturating_duration_since(session.started) >= ADMISSION_TIMEOUT))
        }));
        let finished = self
            .outbound
            .iter()
            .filter(|(_, session)| {
                fabric.speaker_group(session.group).is_none()
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

        let led = self.groups.iter().flatten().cloned().collect::<Vec<_>>();
        for group in led {
            let awaiting = fabric
                .speaker_member_states(&group)
                .into_iter()
                .zip(&group.members)
                .filter(|(state, member)| {
                    *state == SpeakerMemberStatus::AwaitingConsent && online(&member.device)
                })
                .map(|(_, member)| member.device.clone())
                .collect::<BTreeSet<_>>();
            let mut pending = group.clone();
            pending
                .members
                .retain(|member| awaiting.contains(&member.device));
            out.extend(self.invite(&pending, now, false));
        }
        out
    }

    fn end_inbound(&mut self, mut predicate: impl FnMut(&MemberSession) -> bool) -> Vec<Egress> {
        let ids = self
            .inbound
            .iter()
            .filter(|(_, session)| predicate(session))
            .map(|(session_id, _)| *session_id)
            .collect::<BTreeSet<_>>();
        self.end_inbound_ids(&ids)
    }

    fn end_inbound_ids(&mut self, ids: &BTreeSet<[u8; 16]>) -> Vec<Egress> {
        ids.iter()
            .filter_map(|session_id| {
                self.inbound.remove(session_id).map(|session| {
                    egress(
                        &session.source,
                        PersonalFabricAction::SpeakerGroupSessionStop {
                            session_id: *session_id,
                            authorization: Some(session.authorization),
                        },
                    )
                })
            })
            .collect()
    }

    fn end_outbound_for_group(&mut self, group: SpeakerGroupId) -> Vec<Egress> {
        let (ended, kept): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut self.outbound)
            .into_iter()
            .partition(|(_, session)| session.group == group);
        self.outbound = kept;
        ended.values().flat_map(stop_members).collect()
    }

    fn drop_outbound_member(
        &mut self,
        mut predicate: impl FnMut(&GroupSession, &SessionMember) -> bool,
    ) -> Vec<Egress> {
        let mut out = Vec::new();
        for session in self.outbound.values_mut() {
            let snapshot = session.clone();
            for member in &mut session.members {
                if member.phase == SessionMemberPhase::Refused || !predicate(&snapshot, member) {
                    continue;
                }
                if member.phase != SessionMemberPhase::Local {
                    out.push(egress(
                        &member.device,
                        PersonalFabricAction::SpeakerGroupSessionStop {
                            session_id: member.route,
                            authorization: member.authorization,
                        },
                    ));
                }
                member.phase = SessionMemberPhase::Refused;
                member.authorization = None;
            }
        }
        out
    }

    /// Presentation rows for the Devices surface and Settings. `playout`
    /// answers, per member route, whether that member can play in time (see
    /// [`crate::shell::remote_audio::group_route_playouts`]).
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one projection keeps every speaker row beside the fabric state it reads"
    )]
    pub fn view(
        &self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        playout: &dyn Fn([u8; 16]) -> Option<ClockSyncState>,
    ) -> SpeakerGroupsView {
        let label = |device: &DeviceId| {
            fabric.snapshots().get(device).map_or_else(
                || device.as_str().to_string(),
                |snapshot| snapshot.presence.label.clone(),
            )
        };
        let output_label = |device: &DeviceId, output: EndpointSelector| {
            fabric
                .snapshots()
                .get(device)
                .and_then(|snapshot| snapshot.speakers.output(output))
                .map(|endpoint| endpoint.label.clone())
        };
        let groups = fabric
            .speaker_groups()
            .into_iter()
            .map(|group| SpeakerGroupView {
                id: group.id,
                name: group.name.clone(),
                leader: group.leader.clone(),
                leader_label: label(&group.leader),
                led_locally: &group.leader == local,
                members: group
                    .members
                    .iter()
                    .zip(fabric.speaker_member_states(group))
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
            .filter(|(group, pending)| consent_matches(fabric, local, **group, pending))
            .filter_map(|(group, pending)| {
                let published = fabric.speaker_group(*group)?;
                Some(SpeakerConsentView {
                    group: *group,
                    name: published.name.clone(),
                    leader: pending.leader.clone(),
                    leader_label: label(&pending.leader),
                    scope: SpeakerConsentScope::Play(pending.role),
                    endpoint_label: output_label(local, pending.output),
                })
            })
            .chain(self.listen_consent_views(fabric, local))
            .collect();
        let memberships = self
            .memberships
            .iter()
            .flatten()
            .map(|membership| SpeakerMembershipView {
                group: membership.group,
                name: fabric
                    .speaker_group(membership.group)
                    .map_or_else(String::new, |group| group.name.clone()),
                leader_label: label(&membership.leader),
                scope: SpeakerConsentScope::Play(membership.role),
            })
            .chain(self.listener_views(fabric))
            .collect();
        let name_of = |group: SpeakerGroupId| {
            fabric
                .speaker_group(group)
                .map_or_else(String::new, |group| group.name.clone())
        };
        let sessions = self
            .outbound
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
            .collect();
        SpeakerGroupsView {
            groups,
            consents,
            memberships,
            sessions,
        }
    }

    /// What the playing device's timed audio path renders for each session:
    /// only members that are this device's own outputs or have admitted it.
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

    /// Every audio route this device should run right now, derived from the
    /// sessions it plays and the sessions it admitted as a member.
    #[must_use]
    pub fn route_specs(&self, fabric: &DeviceFabric, local: &DeviceId) -> Vec<RouteSpec> {
        let playing = self.outbound.values().flat_map(|session| {
            session.members.iter().filter_map(move |member| {
                match (member.phase, member.authorization) {
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
                }
            })
        });
        let receiving = self.inbound.iter().filter_map(|(route, session)| {
            let grant = self.grant(fabric, local, *route, &session.source)?;
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

    /// The output this device plays a calibration sweep on when `source`
    /// asks: only the group's leader may ask, and only while this device's
    /// consent covers its membership.
    #[must_use]
    pub fn sweep_output(
        &self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        source: &DeviceId,
        group: SpeakerGroupId,
    ) -> Option<EndpointSelector> {
        let published = fabric.speaker_group(group)?;
        (&published.leader == source && self.consent_covers(fabric, local, group))
            .then(|| published.member(local).map(|member| member.output))?
    }

    /// Whether this device admitted `route` from exactly `source`. Checked for
    /// every received frame, so a withdrawn consent stops audio at once.
    #[must_use]
    pub fn admits(&self, route: [u8; 16], source: &DeviceId) -> bool {
        self.inbound
            .get(&route)
            .is_some_and(|session| &session.source == source)
    }

    /// The audio path ended `route` on its own (a lost link or a failed
    /// output): end the member session it carried, telling the peer.
    pub fn route_lost(&mut self, route: [u8; 16]) -> Vec<Egress> {
        if self.inbound.contains_key(&route) {
            return self.end_inbound_ids(&BTreeSet::from([route]));
        }
        self.drop_outbound_member(|_, member| member.route == route)
    }

    /// What a member plays for one admitted session, resolved against the
    /// group as its leader publishes it now.
    #[must_use]
    pub fn grant(
        &self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        session_id: [u8; 16],
        source: &DeviceId,
    ) -> Option<MemberPlaybackGrant> {
        let session = self.inbound.get(&session_id)?;
        if &session.source != source || !self.consent_covers(fabric, local, session.group) {
            return None;
        }
        let group = fabric.speaker_group(session.group)?;
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
    if !crate::shell::speaker_group::valid_group_name(&group.name) {
        return Err(crate::shell::i18n::t("speaker-error-name"));
    }
    group
        .validate(local)
        .map_err(|_| crate::shell::i18n::t("speaker-error-roles"))?;
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

fn stop_members(session: &GroupSession) -> Vec<Egress> {
    session
        .members
        .iter()
        .filter(|member| {
            matches!(
                member.phase,
                SessionMemberPhase::AwaitingAdmission | SessionMemberPhase::Admitted
            )
        })
        .map(|member| {
            egress(
                &member.device,
                PersonalFabricAction::SpeakerGroupSessionStop {
                    session_id: member.route,
                    authorization: member.authorization,
                },
            )
        })
        .collect()
}

fn consent_matches(
    fabric: &DeviceFabric,
    local: &DeviceId,
    group: SpeakerGroupId,
    pending: &PendingConsent,
) -> bool {
    fabric.speaker_group(group).is_some_and(|published| {
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
    /// Play in the group on one of this device's outputs.
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

/// "Join 'Living room' as the right speaker?" or "Let the desk use this
/// microphone to calibrate 'Living room'?" on the asked device.
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
        crate::shell::i18n::t_args(
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
        crate::shell::i18n::t_args(key, &args)
    }

    #[must_use]
    pub fn accessible_name(&self) -> String {
        crate::shell::i18n::t_args(
            self.key("a11y-speaker-join", "a11y-speaker-listen"),
            &self.args(),
        )
    }

    #[must_use]
    pub fn accessible_description(&self) -> String {
        crate::shell::i18n::t_args(
            self.key(
                "a11y-speaker-join-description",
                "a11y-speaker-listen-description",
            ),
            &self.args(),
        )
    }

    #[must_use]
    pub fn allow_label(&self) -> String {
        crate::shell::i18n::t(self.key("a11y-speaker-join-allow", "a11y-speaker-listen-allow"))
    }

    #[must_use]
    pub fn decline_label(&self) -> String {
        crate::shell::i18n::t(self.key("a11y-speaker-join-decline", "a11y-speaker-listen-decline"))
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
        crate::shell::i18n::t_args(key, &self.args())
    }

    #[must_use]
    pub fn action_subtitle(&self) -> String {
        crate::shell::i18n::t_args(
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
        crate::shell::i18n::t_args(
            self.key("a11y-speaker-membership", "a11y-speaker-listener"),
            &self.args(sentence),
        )
    }

    #[must_use]
    pub fn note(&self) -> String {
        crate::shell::i18n::t_args(
            self.key(
                "qs-devices-speaker-membership-note",
                "qs-devices-speaker-listener-note",
            ),
            &self.args(str::to_string),
        )
    }

    #[must_use]
    pub fn accessible_description(&self) -> String {
        crate::shell::i18n::t(self.key(
            "a11y-speaker-membership-description",
            "a11y-speaker-listener-description",
        ))
    }

    #[must_use]
    pub fn leave_title(&self) -> String {
        crate::shell::i18n::t_args(
            self.key(
                "search-continuity-speaker-leave",
                "search-continuity-speaker-listen-stop",
            ),
            &self.args(str::to_string),
        )
    }

    #[must_use]
    pub fn leave_subtitle(&self) -> String {
        crate::shell::i18n::t_args(
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

/// Which routes to end and which to start so the running set becomes
/// `desired`. A route whose parameters changed is restarted.
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
    /// The member-issued capability every frame must carry; `None` for this
    /// device's own output.
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
    crate::shell::i18n::t("speaker-error-unavailable")
}

fn send_all(network: &crate::shell::network::ShellNetwork, out: Vec<Egress>) -> Result<(), String> {
    let mut first_error = None;
    for item in out {
        if let Err(error) = network.send_personal_fabric_action(&item.target, item.action) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn local_device(network: &crate::shell::network::ShellNetwork) -> Result<DeviceId, String> {
    network
        .local_device_fabric_identity()
        .map(|(device, _)| device)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))
}

/// The speaker records for this device's next snapshot. Called by the
/// snapshot builder; publishes nothing while media continuity is off.
pub(crate) fn authored_state(
    device: &DeviceId,
    previous: Option<&SpeakerFabricState>,
    stamp: &FabricStamp,
    media_enabled: bool,
) -> SpeakerFabricState {
    if !media_enabled {
        return SpeakerFabricState::default();
    }
    let endpoints = local_endpoints(device, Instant::now());
    core().lock().map_or_else(
        |_| SpeakerFabricState::default(),
        |mut core| {
            core.seed(previous);
            core.authored(endpoints, stamp)
        },
    )
}

/// Handle one authenticated action. Returns true when it was a
/// speaker-group action, including a rejected one.
pub(crate) fn handle_action(
    network: &crate::shell::network::ShellNetwork,
    envelope: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
) -> bool {
    let Ok(local) = local_device(network) else {
        return false;
    };
    if let PersonalFabricAction::SpeakerIdentify { output } = envelope.action {
        let _ = play_identify_locally(&local, output);
        return true;
    }
    if let PersonalFabricAction::SpeakerSweep { group, start_ns } = envelope.action {
        let fabric = network.device_fabric();
        let output = core()
            .lock()
            .ok()
            .and_then(|core| core.sweep_output(&fabric, &local, &envelope.source, group));
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
        let fabric = network.device_fabric();
        let allowed = core().lock().is_ok_and(|core| {
            core.may_listen(&fabric, &local, &envelope.source, group, microphone)
        });
        crate::shell::speaker_calibration::listen_for_leader(
            &local,
            allowed,
            crate::shell::speaker_calibration::ListenRequest {
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
        crate::shell::speaker_calibration::deliver_hearing(
            &envelope.source,
            *group,
            *sweep,
            hearing.clone(),
        );
        return true;
    }
    let fabric = network.device_fabric();
    let out = {
        let Ok(mut core) = core().lock() else {
            return false;
        };
        core.seed(
            fabric
                .snapshots()
                .get(&local)
                .map(|snapshot| &snapshot.speakers),
        );
        core.handle(
            &fabric,
            &local,
            &envelope.source,
            &envelope.action,
            Instant::now(),
            &mut crate::random::nonzero::<16>,
        )
    };
    match out {
        Some(out) => {
            let _ = send_all(network, out);
            true
        }
        None => false,
    }
}

/// Play the identify sound on `output` of `device`: locally, or by asking
/// that device to play it on its own output.
///
/// # Errors
///
/// Returns a message when the device or output is unavailable, a sound
/// played too recently, or the request cannot be delivered.
pub fn identify(
    network: &crate::shell::network::ShellNetwork,
    device: &DeviceId,
    output: EndpointSelector,
) -> Result<(), String> {
    let local = local_device(network)?;
    if device == &local {
        return play_identify_locally(&local, output);
    }
    let fabric = network.device_fabric();
    let offered = fabric.snapshots().get(device).is_some_and(|snapshot| {
        snapshot.presence.online && snapshot.speakers.output(output).is_some()
    });
    if !offered {
        return Err(crate::shell::i18n::t("speaker-error-speaker-unavailable"));
    }
    network
        .send_personal_fabric_action(device, PersonalFabricAction::SpeakerIdentify { output })
        .map(|_| ())
}

fn play_identify_locally(local: &DeviceId, output: EndpointSelector) -> Result<(), String> {
    let sink = resolve_local_output(local, output)
        .ok_or_else(|| crate::shell::i18n::t("speaker-error-output-disconnected"))?;
    if !crate::shell::speaker_settings::admit_identify(Instant::now()) {
        return Err(crate::shell::i18n::t("speaker-error-already-playing"));
    }
    std::thread::Builder::new()
        .name("speaker-identify".into())
        .spawn(move || play_pcm(&sink.name, &identify_chirp()))
        .map(|_| ())
        .map_err(|error| {
            crate::shell::i18n::t_args(
                "speaker-error-play-failed",
                &[("error", error.to_string().into())],
            )
        })
}

/// Play the calibration sweep on `output` when this device's clock reads
/// `start_ns`; the leader already converted its schedule through its
/// estimate of this clock. One sweep at a time; requests too far from now
/// are ignored, and the leader's measurement then reports no sweep.
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
            if let Ok(sweep) = crate::shell::speaker_calibration::sweep() {
                let _ = media::pw_stream::TimedAudio::native().play_at(
                    sink.serial,
                    crate::shell::speaker_calibration::SWEEP_RATE,
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

fn play_pcm(sink_name: &str, samples: &[i16]) {
    use std::io::Write as _;
    let child = std::process::Command::new("pw-play")
        .args([
            "--target",
            sink_name,
            "--media-type",
            "Audio",
            "--media-category",
            "Playback",
            "--rate",
            "48000",
            "--channels",
            "1",
            "--format",
            "s16",
            "--raw",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>();
        let _ = stdin.write_all(&bytes);
    }
    let _ = child.wait();
}

/// Periodic maintenance from the fabric runtime's pump.
pub(crate) fn pump(network: &crate::shell::network::ShellNetwork, media_enabled: bool) {
    let Ok(local) = local_device(network) else {
        return;
    };
    let fabric = network.device_fabric();
    let out = core().lock().map_or_else(
        |_| Vec::new(),
        |mut core| {
            if media_enabled {
                core.seed(
                    fabric
                        .snapshots()
                        .get(&local)
                        .map(|snapshot| &snapshot.speakers),
                );
            }
            core.pump(&fabric, &local, media_enabled, Instant::now())
        },
    );
    let _ = send_all(network, out);
    let answers = crate::shell::speaker_calibration::drain_answers();
    if media_enabled {
        for (leader, action) in answers {
            let _ = network.send_personal_fabric_action(&leader, action);
        }
    }
    reconcile_routes(network, &fabric, &local, media_enabled);
    crate::shell::speaker_settings::pump(network);
}

/// Routes this process started, routes starting on a worker, and routes
/// whose start failed recently.
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

/// Bring the audio routes in line with the sessions: start routes for newly
/// admitted members, end routes whose session or consent ended, and end the
/// session of a route the audio path dropped on its own.
fn reconcile_routes(
    network: &crate::shell::network::ShellNetwork,
    fabric: &DeviceFabric,
    local: &DeviceId,
    media_enabled: bool,
) {
    let desired = if media_enabled {
        core()
            .lock()
            .map_or_else(|_| Vec::new(), |core| core.route_specs(fabric, local))
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
        .filter(|route| !crate::shell::remote_audio::has_group_route(*route))
        .collect::<Vec<_>>();
    for route in &lost {
        routes.running.remove(route);
    }
    let (end, start) = route_changes(&routes.running, &desired);
    for route in end {
        routes.running.remove(&route);
        crate::shell::remote_audio::end_group_route(route);
    }
    let now = Instant::now();
    routes.retry_after.retain(|_, until| *until > now);
    for spec in start {
        let route = spec.route();
        if routes.retry_after.contains_key(&route) || !routes.starting.insert(route) {
            continue;
        }
        // Opening sinks and streams talks to the audio daemon; never on the
        // compositor thread.
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
        let _ = send_all(network, out);
    }
}

fn audio_source(source: &GroupAudioSource) -> crate::shell::remote_audio::GroupRouteSource {
    match source {
        GroupAudioSource::PickerToken(token) => {
            crate::shell::remote_audio::GroupRouteSource::Picker(*token)
        }
        // The media keeps decoding here, so everything this device plays is
        // what the group should hear.
        GroupAudioSource::MediaSession(_) => crate::shell::remote_audio::GroupRouteSource::System,
    }
}

fn member_output(local: &DeviceId, output: EndpointSelector) -> Result<u32, String> {
    resolve_local_output(local, output)
        .map(|sink| sink.serial)
        .ok_or_else(|| crate::shell::i18n::t("speaker-error-speaker-output-gone"))
}

fn start_route(local: &DeviceId, spec: &RouteSpec) -> Result<(), String> {
    use crate::shell::remote_audio::{
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

/// Whether this device admitted `route` from exactly `source`; the live
/// audio path checks it for every group frame.
#[must_use]
pub fn admits_route(route: [u8; 16], source: &DeviceId) -> bool {
    core().lock().is_ok_and(|core| core.admits(route, source))
}

fn with_core<T>(
    network: &crate::shell::network::ShellNetwork,
    apply: impl FnOnce(
        &mut SpeakerGroupCore,
        &DeviceFabric,
        &DeviceId,
    ) -> Result<(T, Vec<Egress>), String>,
) -> Result<T, String> {
    let local = local_device(network)?;
    let fabric = network.device_fabric();
    let mut core = core().lock().map_err(|_| unavailable())?;
    core.seed(
        fabric
            .snapshots()
            .get(&local)
            .map(|snapshot| &snapshot.speakers),
    );
    let applied = apply(&mut core, &fabric, &local);
    drop(core);
    let (value, out) = applied?;
    // Local state already changed; a member that cannot be reached now is
    // asked again by the next pump, so delivery failures are not fatal.
    let _ = send_all(network, out);
    Ok(value)
}

/// Create a group led by this device.
///
/// # Errors
///
/// Returns a message for an invalid or inadmissible group.
pub fn create_group(
    network: &crate::shell::network::ShellNetwork,
    name: &str,
    members: &[MemberSpec],
) -> Result<SpeakerGroupId, String> {
    with_core(network, |core, fabric, local| {
        let id = SpeakerGroupId(crate::random::nonzero::<16>());
        let out = core.create_group(fabric, local, id, name, members, Instant::now())?;
        Ok((id, out))
    })
}

/// Rename or re-member a group led by this device.
///
/// # Errors
///
/// Returns a message for an unknown group or an inadmissible member set.
pub fn update_group(
    network: &crate::shell::network::ShellNetwork,
    id: SpeakerGroupId,
    name: &str,
    members: &[MemberSpec],
) -> Result<(), String> {
    with_core(network, |core, fabric, local| {
        let out = core.update_group(fabric, local, id, name, members, Instant::now())?;
        Ok(((), out))
    })
}

/// Store calibration trims on a group led by this device.
///
/// # Errors
///
/// Returns a message for an unknown group or member, or out-of-range trims.
pub fn set_trims(
    network: &crate::shell::network::ShellNetwork,
    id: SpeakerGroupId,
    trims: &[(DeviceId, EndpointSelector, SpeakerTrim)],
) -> Result<(), String> {
    with_core(network, |core, _, _| {
        core.set_trims(id, trims)?;
        Ok(((), Vec::new()))
    })
}

/// Delete a group led by this device.
///
/// # Errors
///
/// Returns a message for an unknown group.
pub fn delete_group(
    network: &crate::shell::network::ShellNetwork,
    id: SpeakerGroupId,
) -> Result<(), String> {
    with_core(network, |core, _, _| Ok(((), core.delete_group(id)?)))
}

/// Answer a join or microphone prompt on this device.
///
/// # Errors
///
/// Returns a message for an expired or changed request.
pub fn resolve_consent(
    network: &crate::shell::network::ShellNetwork,
    group: SpeakerGroupId,
    kind: SpeakerConsentKind,
    approved: bool,
) -> Result<(), String> {
    with_core(network, |core, fabric, local| {
        let stamp = FabricStamp {
            counter: 0,
            device: local.clone(),
        };
        let out = match kind {
            SpeakerConsentKind::Play => {
                core.resolve_consent(fabric, local, group, approved, &stamp)?
            }
            SpeakerConsentKind::Listen => {
                core.resolve_listen_consent(fabric, local, group, approved, &stamp)?
            }
        };
        Ok(((), out))
    })
}

/// Leave a group led by another device, or stop letting it listen with
/// this device's microphone.
///
/// # Errors
///
/// Returns a message when this device gave no such consent.
pub fn leave_group(
    network: &crate::shell::network::ShellNetwork,
    group: SpeakerGroupId,
    kind: SpeakerConsentKind,
) -> Result<(), String> {
    with_core(network, |core, _, _| {
        let out = match kind {
            SpeakerConsentKind::Play => core.leave(group)?,
            SpeakerConsentKind::Listen => core.stop_listening(group)?,
        };
        Ok(((), out))
    })
}

/// The person's microphones that could listen while calibrating a group
/// this device leads.
#[must_use]
pub fn listener_candidates(
    network: &crate::shell::network::ShellNetwork,
    group: &SpeakerGroup,
) -> Vec<ListenerCandidate> {
    let Ok(local) = local_device(network) else {
        return Vec::new();
    };
    let fabric = network.device_fabric();
    core().lock().map_or_else(
        |_| Vec::new(),
        |core| core.listener_candidates(&fabric, &local, group, Instant::now()),
    )
}

/// Ask the devices of `microphones` that have not answered yet to let this
/// device listen while calibrating `group`.
///
/// # Errors
///
/// Returns a message when the speaker runtime is unavailable.
pub fn invite_listeners(
    network: &crate::shell::network::ShellNetwork,
    group: &SpeakerGroup,
    microphones: &[(DeviceId, EndpointSelector)],
) -> Result<(), String> {
    with_core(network, |core, _, _| {
        Ok((
            (),
            core.invite_listeners(group, microphones, Instant::now()),
        ))
    })
}

/// Play `source` from this device to a group.
///
/// # Errors
///
/// Returns a message when the group is unavailable or no member is ready.
pub fn start_session(
    network: &crate::shell::network::ShellNetwork,
    group: SpeakerGroupId,
    source: GroupAudioSource,
) -> Result<[u8; 16], String> {
    with_core(network, |core, fabric, local| {
        let session_id = crate::random::nonzero::<16>();
        let out = core.start_session(fabric, local, group, source, session_id, Instant::now())?;
        Ok((session_id, out))
    })
}

/// Stop one session in either direction.
///
/// # Errors
///
/// Returns a message when the session already ended.
pub fn stop_session(
    network: &crate::shell::network::ShellNetwork,
    session_id: [u8; 16],
) -> Result<(), String> {
    with_core(network, |core, _, _| {
        Ok(((), core.stop_session(session_id)?))
    })
}

#[must_use]
pub fn view(network: &crate::shell::network::ShellNetwork) -> SpeakerGroupsView {
    let Ok(local) = local_device(network) else {
        return SpeakerGroupsView::default();
    };
    let fabric = network.device_fabric();
    // Read before taking the core: the audio path takes the core while it
    // holds its own state.
    let playouts = crate::shell::remote_audio::group_route_playouts();
    core().lock().map_or_else(
        |_| SpeakerGroupsView::default(),
        |core| core.view(&fabric, &local, &|route| playouts.get(&route).copied()),
    )
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
        while label.len() > crate::shell::speaker_group::MAX_AUDIO_ENDPOINT_LABEL_BYTES {
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
        .take(crate::shell::speaker_group::MAX_AUDIO_ENDPOINTS)
        .collect()
}

#[derive(Default)]
struct EndpointCache {
    endpoints: Option<(Instant, Vec<AudioEndpoint>)>,
    refreshing: bool,
}

fn endpoint_cache() -> &'static Mutex<EndpointCache> {
    static CACHE: OnceLock<Mutex<EndpointCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(EndpointCache::default()))
}

/// The cached endpoint list; a stale list is refreshed on a worker thread so
/// the compositor never waits on the audio graph.
fn local_endpoints(local: &DeviceId, now: Instant) -> Vec<AudioEndpoint> {
    let Ok(mut cache) = endpoint_cache().lock() else {
        return Vec::new();
    };
    let stale = cache
        .endpoints
        .as_ref()
        .is_none_or(|(updated, _)| now.saturating_duration_since(*updated) >= ENDPOINT_REFRESH);
    if stale && !cache.refreshing {
        cache.refreshing = true;
        let local = local.clone();
        let spawned = std::thread::Builder::new()
            .name("speaker-endpoints".into())
            .spawn(move || {
                let endpoints = endpoints_from_graph(&local, &media::audio::enumerate());
                if let Ok(mut cache) = endpoint_cache().lock() {
                    cache.endpoints = Some((Instant::now(), endpoints));
                    cache.refreshing = false;
                }
            });
        if spawned.is_err() {
            cache.refreshing = false;
        }
    }
    cache
        .endpoints
        .as_ref()
        .map_or_else(Vec::new, |(_, endpoints)| endpoints.clone())
}

#[cfg(test)]
mod tests;
