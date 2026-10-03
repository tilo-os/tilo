//! Consent for calibration to listen with another device's microphone.
//!
//! It mirrors membership consent. The leader invites a device when a
//! calibration first wants its microphone; that device asks its own user
//! once and, on approval, authors a [`SpeakerListener`] in its own snapshot.
//! Only then may the leader ask it to record around a sweep, and the device
//! answers with what it measured from the recording, never the recording.
//! The leader's own microphones need no consent: the person calibrating is
//! at that device.

use std::collections::BTreeSet;
use std::time::Instant;

use super::{
    CONSENT_TTL, Egress, SpeakerConsentScope, SpeakerConsentView, SpeakerGroupCore, egress,
};
use crate::shell::device_fabric::{DeviceFabric, FabricStamp, PersonalFabricAction};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::speaker_group::{
    EndpointSelector, MAX_SPEAKER_LISTENERS, SpeakerGroup, SpeakerGroupId, SpeakerListener,
};

#[derive(Clone, Debug)]
pub(super) struct PendingListen {
    leader: DeviceId,
    microphone: EndpointSelector,
    received: Instant,
}

#[derive(Clone, Debug)]
pub(super) struct ListenInvite {
    microphone: EndpointSelector,
    sent: Instant,
    answer: Option<bool>,
}

/// Where one device's microphone stands for calibrating one group, as its
/// leader sees it.
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
    fn listeners_mut(&mut self) -> &mut Vec<SpeakerListener> {
        self.listeners.get_or_insert_with(Vec::new)
    }

    /// Every device of the person with a microphone, one microphone each,
    /// this device first.
    #[must_use]
    pub fn listener_candidates(
        &self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        group: &SpeakerGroup,
        now: Instant,
    ) -> Vec<ListenerCandidate> {
        let mut candidates = fabric
            .snapshots()
            .values()
            .filter_map(|snapshot| {
                let microphone = snapshot.speakers.calibration_microphone()?;
                let device = snapshot.owner.clone();
                let recent =
                    self.listen_invites
                        .get(&(group.id, device.clone()))
                        .filter(|invite| {
                            invite.microphone == microphone.selector
                                && now.saturating_duration_since(invite.sent) < CONSENT_TTL
                        });
                let consented = snapshot
                    .speakers
                    .listeners
                    .iter()
                    .any(|listener| listener.covers(group, microphone.selector))
                    || recent.is_some_and(|invite| invite.answer == Some(true));
                let state = if &device == local {
                    ListenerState::Ready
                } else if !snapshot.presence.capabilities.audio_group {
                    ListenerState::Unavailable
                } else if !snapshot.presence.online {
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
                    device_label: snapshot.presence.label.clone(),
                    microphone: microphone.selector,
                    label: microphone.label.clone(),
                    state,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| !candidate.local);
        candidates
    }

    /// Ask each of `microphones` that has not answered recently to let this
    /// device listen while calibrating `group`.
    pub fn invite_listeners(
        &mut self,
        group: &SpeakerGroup,
        microphones: &[(DeviceId, EndpointSelector)],
        now: Instant,
    ) -> Vec<Egress> {
        microphones
            .iter()
            .filter(|(device, _)| device != &group.leader)
            .filter_map(|(device, microphone)| {
                let key = (group.id, device.clone());
                let answered = self.listen_invites.get(&key).is_some_and(|invite| {
                    invite.microphone == *microphone
                        && now.saturating_duration_since(invite.sent) < CONSENT_TTL
                });
                if answered {
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
                Some(egress(
                    device,
                    PersonalFabricAction::SpeakerListenInvite {
                        group: group.id,
                        name: group.name.clone(),
                        microphone: *microphone,
                    },
                ))
            })
            .collect()
    }

    /// Answer a microphone prompt on this device.
    ///
    /// # Errors
    ///
    /// Returns a message when the prompt expired, the group changed, or too
    /// many groups may already listen here.
    pub fn resolve_listen_consent(
        &mut self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        group_id: SpeakerGroupId,
        approved: bool,
        stamp: &FabricStamp,
    ) -> Result<Vec<Egress>, String> {
        let pending = self
            .listen_consents
            .remove(&group_id)
            .ok_or_else(|| crate::shell::i18n::t("speaker-error-request-expired"))?;
        if approved {
            if !listen_matches(fabric, local, group_id, &pending) {
                return Err(crate::shell::i18n::t("speaker-error-group-changed"));
            }
            let listeners = self.listeners_mut();
            listeners.retain(|listener| listener.group != group_id);
            if listeners.len() >= MAX_SPEAKER_LISTENERS {
                self.listen_consents.insert(group_id, pending);
                return Err(crate::shell::i18n::t("speaker-error-too-many-memberships"));
            }
            listeners.push(SpeakerListener {
                group: group_id,
                leader: pending.leader.clone(),
                microphone: pending.microphone,
                stamp: stamp.clone(),
            });
        }
        Ok(vec![egress(
            &pending.leader,
            PersonalFabricAction::SpeakerListenDecision {
                group: group_id,
                accepted: approved,
            },
        )])
    }

    /// Withdraw this device's microphone consent for a group.
    ///
    /// # Errors
    ///
    /// Returns a message when no such consent exists.
    pub fn stop_listening(&mut self, group_id: SpeakerGroupId) -> Result<Vec<Egress>, String> {
        let listeners = self.listeners_mut();
        let index = listeners
            .iter()
            .position(|listener| listener.group == group_id)
            .ok_or_else(|| crate::shell::i18n::t("speaker-error-not-a-member"))?;
        let listener = listeners.remove(index);
        Ok(vec![egress(
            &listener.leader,
            PersonalFabricAction::SpeakerListenDecision {
                group: group_id,
                accepted: false,
            },
        )])
    }

    /// Whether `source` may have this device record `microphone` for
    /// `group` now: it leads the group as published, and this device's
    /// consent covers exactly that group, leader and microphone.
    #[must_use]
    pub fn may_listen(
        &self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        source: &DeviceId,
        group: SpeakerGroupId,
        microphone: EndpointSelector,
    ) -> bool {
        fabric.speaker_group(group).is_some_and(|published| {
            &published.leader == source
                && source != local
                && self
                    .listeners
                    .iter()
                    .flatten()
                    .any(|listener| listener.covers(published, microphone))
        })
    }

    /// The leader removed the group, or this device left: drop any
    /// microphone consent and prompt for it.
    pub(super) fn forget_listening(&mut self, group: SpeakerGroupId, source: &DeviceId) {
        if let Some(listeners) = self.listeners.as_mut() {
            listeners.retain(|listener| listener.group != group || &listener.leader != source);
        }
        if self
            .listen_consents
            .get(&group)
            .is_some_and(|pending| &pending.leader == source)
        {
            self.listen_consents.remove(&group);
        }
        self.listen_invites.remove(&(group, source.clone()));
    }

    pub(super) fn handle_listen_action(
        &mut self,
        source: &DeviceId,
        action: &PersonalFabricAction,
        now: Instant,
    ) -> Option<Vec<Egress>> {
        match action {
            PersonalFabricAction::SpeakerListenInvite {
                group, microphone, ..
            } => {
                let already = self.listeners.iter().flatten().any(|listener| {
                    listener.group == *group
                        && &listener.leader == source
                        && listener.microphone == *microphone
                });
                if already {
                    return Some(vec![egress(
                        source,
                        PersonalFabricAction::SpeakerListenDecision {
                            group: *group,
                            accepted: true,
                        },
                    )]);
                }
                if self.listen_consents.len() < MAX_SPEAKER_LISTENERS
                    || self.listen_consents.contains_key(group)
                {
                    self.listen_consents.insert(
                        *group,
                        PendingListen {
                            leader: source.clone(),
                            microphone: *microphone,
                            received: now,
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

    /// Maintenance for microphone consent: everything is withdrawn while
    /// media continuity is off; otherwise consent for a group its leader no
    /// longer publishes is void and stale prompts expire.
    pub(super) fn pump_listening(
        &mut self,
        fabric: &DeviceFabric,
        local: &DeviceId,
        enabled: bool,
        now: Instant,
    ) -> Vec<Egress> {
        if !enabled {
            self.listen_consents.clear();
            self.listen_invites.clear();
            return self
                .listeners
                .as_mut()
                .map(|listeners| {
                    listeners
                        .drain(..)
                        .map(|listener| {
                            egress(
                                &listener.leader,
                                PersonalFabricAction::SpeakerListenDecision {
                                    group: listener.group,
                                    accepted: false,
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        let stale = self
            .listeners
            .iter()
            .flatten()
            .filter(|listener| {
                fabric.snapshots().contains_key(&listener.leader)
                    && fabric
                        .speaker_group(listener.group)
                        .is_none_or(|group| group.leader != listener.leader)
            })
            .map(|listener| listener.group)
            .collect::<BTreeSet<_>>();
        if !stale.is_empty() {
            self.listeners_mut()
                .retain(|listener| !stale.contains(&listener.group));
        }
        self.listen_consents.retain(|group, pending| {
            now.saturating_duration_since(pending.received) < CONSENT_TTL
                && listen_matches(fabric, local, *group, pending)
        });
        Vec::new()
    }

    pub(super) fn listen_consent_views(
        &self,
        fabric: &DeviceFabric,
        local: &DeviceId,
    ) -> Vec<SpeakerConsentView> {
        let microphone_label = |microphone| {
            fabric
                .snapshots()
                .get(local)
                .and_then(|snapshot| snapshot.speakers.microphone(microphone))
                .map(|endpoint| endpoint.label.clone())
        };
        self.listen_consents
            .iter()
            .filter(|(group, pending)| listen_matches(fabric, local, **group, pending))
            .filter_map(|(group, pending)| {
                let published = fabric.speaker_group(*group)?;
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

    pub(super) fn listener_views(
        &self,
        fabric: &DeviceFabric,
    ) -> Vec<super::SpeakerMembershipView> {
        self.listeners
            .iter()
            .flatten()
            .map(|listener| super::SpeakerMembershipView {
                group: listener.group,
                name: fabric
                    .speaker_group(listener.group)
                    .map_or_else(String::new, |group| group.name.clone()),
                leader_label: device_label(fabric, &listener.leader),
                scope: SpeakerConsentScope::Listen,
            })
            .collect()
    }
}

fn device_label(fabric: &DeviceFabric, device: &DeviceId) -> String {
    fabric.snapshots().get(device).map_or_else(
        || device.as_str().to_string(),
        |snapshot| snapshot.presence.label.clone(),
    )
}

/// The prompt still describes the group as its leader publishes it, and
/// this device still offers the microphone.
fn listen_matches(
    fabric: &DeviceFabric,
    local: &DeviceId,
    group: SpeakerGroupId,
    pending: &PendingListen,
) -> bool {
    let published = fabric
        .speaker_group(group)
        .is_some_and(|published| published.leader == pending.leader);
    // Before this device's own snapshot is known, the microphone cannot be
    // checked; the capture resolves it again anyway.
    let offered = fabric
        .snapshots()
        .get(local)
        .is_none_or(|snapshot| snapshot.speakers.microphone(pending.microphone).is_some());
    published && offered
}
