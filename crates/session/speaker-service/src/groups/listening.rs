//! Consent for calibration to listen with another device's microphone.
//!
//! It mirrors membership consent. The leader asks a device when a calibration first wants its
//! microphone; that device asks its own user once and, on approval, writes a listening consent
//! into its own part of the record of the person's devices. Only then may the leader ask it to
//! capture around a sweep, and the device answers with what it measured from that audio, never
//! the audio itself. The leader's own microphones need no consent: the person calibrating is at
//! that device.

use std::collections::BTreeSet;
use std::time::Instant;

use super::{
    CONSENT_TTL, ConsentWrite, SpeakerConsentScope, SpeakerConsentView, SpeakerGroupCore,
    SpeakerState, SpeakerWrite,
};
use tilo_network_actor::fabric::DeviceId;
use tilo_network_actor::fabric::PersonalFabricAction;
use tilo_network_actor::fabric::speaker_group::{
    EndpointSelector, SpeakerGroup, SpeakerGroupId, SpeakerListener,
};

use crate::devices::Listing;

/// One live ask to a device to let this one listen while calibrating.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenEgress {
    pub target: DeviceId,
    pub action: PersonalFabricAction,
}

#[derive(Clone, Debug)]
pub struct PendingListen {
    pub(super) leader: DeviceId,
    pub(super) microphone: EndpointSelector,
}

#[derive(Clone, Debug)]
pub struct ListenInvite {
    microphone: EndpointSelector,
    sent: Instant,
    answer: Option<bool>,
}

/// Where one device's microphone stands for calibrating one group, as its leader sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerState {
    /// This device, or a device whose consent covers the group.
    Ready,
    /// Will be asked on that device when a calibration wants it.
    NotAsked,
    AwaitingConsent,
    Declined,
    Offline,
    /// Media continuity is off there.
    Unavailable,
}

/// One device's calibration microphone, as the leader's Settings shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerCandidate {
    pub device: DeviceId,
    pub device_label: String,
    pub microphone: EndpointSelector,
    pub label: String,
    pub local: bool,
    pub state: ListenerState,
}

impl SpeakerGroupCore {
    /// Every device of the person with a microphone, one microphone each, this device first.
    #[must_use]
    pub fn listener_candidates(
        &self,
        state: &SpeakerState,
        fabric: &Listing,
        local: &DeviceId,
        group: &SpeakerGroup,
        now: Instant,
    ) -> Vec<ListenerCandidate> {
        let mut candidates = fabric
            .devices()
            .values()
            .filter_map(|known| {
                let microphone = state.calibration_microphone(&known.device)?;
                let device = known.device.clone();
                let recent =
                    self.listen_invites
                        .get(&(group.id, device.clone()))
                        .filter(|invite| {
                            invite.microphone == microphone.selector
                                && now.saturating_duration_since(invite.sent) < CONSENT_TTL
                        });
                let consented = state
                    .listeners_of(&device)
                    .iter()
                    .any(|listener| listener.covers(group, microphone.selector))
                    || recent.is_some_and(|invite| invite.answer == Some(true));
                let state = if &device == local {
                    ListenerState::Ready
                } else if !known.about.capabilities.audio_group {
                    ListenerState::Unavailable
                } else if !known.online {
                    ListenerState::Offline
                } else if consented {
                    ListenerState::Ready
                } else {
                    match recent.map(|invite| invite.answer) {
                        Some(Some(false)) => ListenerState::Declined,
                        Some(_) => ListenerState::AwaitingConsent,
                        None => ListenerState::NotAsked,
                    }
                };
                Some(ListenerCandidate {
                    local: &device == local,
                    device,
                    device_label: known.about.label.clone(),
                    microphone: microphone.selector,
                    label: microphone.label.clone(),
                    state,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| !candidate.local);
        candidates
    }

    /// Ask each of `microphones` that has not answered recently to let this device listen while
    /// calibrating `group`.
    pub fn invite_listeners(
        &mut self,
        state: &SpeakerState,
        group: &SpeakerGroup,
        microphones: &[(DeviceId, EndpointSelector)],
        now: Instant,
    ) -> Vec<ListenEgress> {
        microphones
            .iter()
            .filter(|(device, _)| device != &group.leader)
            .filter_map(|(device, microphone)| {
                let key = (group.id, device.clone());
                let answered = state
                    .listeners_of(device)
                    .iter()
                    .any(|listener| listener.covers(group, *microphone));
                let recent = self.listen_invites.get(&key).is_some_and(|invite| {
                    invite.microphone == *microphone
                        && now.saturating_duration_since(invite.sent) < CONSENT_TTL
                });
                if answered || recent {
                    return None;
                }
                self.listen_invites.insert(
                    key,
                    ListenInvite {
                        microphone: *microphone,
                        sent: now,
                        answer: None,
                    },
                );
                Some(ListenEgress {
                    target: device.clone(),
                    action: PersonalFabricAction::SpeakerListenInvite {
                        group: group.id,
                        name: group.name.clone(),
                        microphone: *microphone,
                    },
                })
            })
            .collect()
    }

    /// Answer a microphone prompt on this device.
    pub fn resolve_listen_consent(
        &mut self,
        state: &SpeakerState,
        local: &DeviceId,
        group_id: SpeakerGroupId,
        approved: bool,
        now: Instant,
    ) -> Result<Vec<SpeakerWrite>, String> {
        let pending = self
            .listen_consents
            .remove(&group_id)
            .ok_or_else(|| crate::i18n::t("speaker-error-request-expired"))?;
        if approved
            && (state
                .group(group_id)
                .is_none_or(|published| published.leader != pending.leader)
                || state.microphone(local, pending.microphone).is_none())
        {
            return Err(crate::i18n::t("speaker-error-group-changed"));
        }
        let _ = now;
        Ok(vec![SpeakerWrite::Consent {
            device: local.clone(),
            consent: ConsentWrite::Listen {
                group: group_id,
                leader: pending.leader,
                microphone: pending.microphone,
            },
            approved,
        }])
    }

    /// Withdraw this device's microphone consent for a group.
    pub fn stop_listening(
        &mut self,
        state: &SpeakerState,
        local: &DeviceId,
        group_id: SpeakerGroupId,
    ) -> Result<Vec<SpeakerWrite>, String> {
        let listener = state
            .listeners_of(local)
            .iter()
            .find(|listener| listener.group == group_id)
            .ok_or_else(|| crate::i18n::t("speaker-error-not-a-member"))?;
        Ok(vec![SpeakerWrite::Withdraw {
            device: local.clone(),
            group: group_id,
            leader: listener.leader.clone(),
            play: false,
        }])
    }

    /// Whether `source` may have this device record `microphone` for `group` now: it leads the
    /// group as written, and this device's consent covers exactly that group, leader and
    /// microphone.
    #[must_use]
    pub fn may_listen(
        &self,
        state: &SpeakerState,
        fabric: &Listing,
        local: &DeviceId,
        source: &DeviceId,
        group: SpeakerGroupId,
        microphone: EndpointSelector,
    ) -> bool {
        let _ = fabric;
        state.group(group).is_some_and(|published| {
            &published.leader == source
                && source != local
                && state
                    .listeners_of(local)
                    .iter()
                    .any(|listener: &SpeakerListener| listener.covers(published, microphone))
        })
    }

    /// The leader removed the group, or this device left: drop the prompt for it.
    pub fn forget_listening(&mut self, group: SpeakerGroupId, source: &DeviceId) {
        if self
            .listen_consents
            .get(&group)
            .is_some_and(|pending| &pending.leader == source)
        {
            self.listen_consents.remove(&group);
        }
        self.listen_invites.remove(&(group, source.clone()));
    }

    pub fn handle_listen_action(
        &mut self,
        state: &SpeakerState,
        local: &DeviceId,
        source: &DeviceId,
        action: &PersonalFabricAction,
        now: Instant,
    ) -> Option<Vec<ListenEgress>> {
        match action {
            PersonalFabricAction::SpeakerListenInvite {
                group, microphone, ..
            } => {
                let local = local.clone();
                let covered = state.listeners_of(&local).iter().any(|listener| {
                    listener.group == *group
                        && &listener.leader == source
                        && listener.microphone == *microphone
                });
                if covered {
                    return Some(vec![ListenEgress {
                        target: source.clone(),
                        action: PersonalFabricAction::SpeakerListenDecision {
                            group: *group,
                            accepted: true,
                        },
                    }]);
                }
                if self.listen_consents.len()
                    < tilo_network_actor::fabric::speaker_group::MAX_SPEAKER_LISTENERS
                    || self.listen_consents.contains_key(group)
                {
                    self.listen_consents.insert(
                        *group,
                        PendingListen {
                            leader: source.clone(),
                            microphone: *microphone,
                        },
                    );
                }
                Some(Vec::new())
            }
            PersonalFabricAction::SpeakerListenDecision { group, accepted } => {
                if let Some(invite) = self.listen_invites.get_mut(&(*group, source.clone())) {
                    invite.answer = Some(*accepted);
                    invite.sent = now;
                }
                Some(Vec::new())
            }
            _ => None,
        }
    }

    /// Maintenance for microphone consent: everything is withdrawn while media continuity is
    /// off; otherwise consent for a group its leader no longer writes is void and stale prompts
    /// expire.
    pub fn pump_listening(
        &mut self,
        state: &SpeakerState,
        fabric: &Listing,
        local: &DeviceId,
        enabled: bool,
        now: Instant,
    ) -> Vec<SpeakerWrite> {
        let _ = now;
        if !enabled {
            self.listen_consents.clear();
            self.listen_invites.clear();
            return Vec::new();
        }
        let voided = state
            .listeners_of(local)
            .iter()
            .filter(|listener| {
                fabric.contains(&listener.leader)
                    && state
                        .group(listener.group)
                        .is_none_or(|group| group.leader != listener.leader)
            })
            .map(|listener| (listener.group, listener.leader.clone()))
            .collect::<BTreeSet<_>>();
        voided
            .into_iter()
            .map(|(group, leader)| SpeakerWrite::Withdraw {
                device: local.clone(),
                group,
                leader,
                play: false,
            })
            .collect()
    }

    pub fn listen_consent_views(
        &self,
        state: &SpeakerState,
        fabric: &Listing,
        local: &DeviceId,
    ) -> Vec<SpeakerConsentView> {
        let microphone_label = |microphone| {
            state
                .microphone(local, microphone)
                .map(|endpoint| endpoint.label.clone())
        };
        self.listen_consents
            .iter()
            .filter(|(group, pending)| listen_matches(state, local, **group, pending))
            .filter_map(|(group, pending)| {
                let published = state.group(*group)?;
                Some(SpeakerConsentView {
                    group: *group,
                    name: published.name.clone(),
                    leader: pending.leader.clone(),
                    leader_label: device_label(fabric, &pending.leader),
                    scope: SpeakerConsentScope::Listen,
                    endpoint_label: microphone_label(pending.microphone),
                })
            })
            .collect()
    }

    pub fn listener_views(
        &self,
        state: &SpeakerState,
        fabric: &Listing,
        local: &DeviceId,
    ) -> Vec<super::SpeakerMembershipView> {
        state
            .listeners_of(local)
            .iter()
            .map(|listener| super::SpeakerMembershipView {
                group: listener.group,
                name: state
                    .group(listener.group)
                    .map_or_else(String::new, |group| group.name.clone()),
                leader_label: device_label(fabric, &listener.leader),
                scope: SpeakerConsentScope::Listen,
            })
            .collect()
    }
}

fn device_label(fabric: &Listing, device: &DeviceId) -> String {
    fabric.get(device).map_or_else(
        || device.as_str().to_string(),
        |known| known.about.label.clone(),
    )
}

/// The prompt still describes the group as its leader writes it, and this device still offers
/// the microphone.
fn listen_matches(
    state: &SpeakerState,
    local: &DeviceId,
    group: SpeakerGroupId,
    pending: &PendingListen,
) -> bool {
    let published = state
        .group(group)
        .is_some_and(|published| published.leader == pending.leader);
    published && state.microphone(local, pending.microphone).is_some()
}
