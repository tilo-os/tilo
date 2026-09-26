//! Consent-bound personal-device tethering.
//!
//! A local hotspot is an ordinary `NetworkManager` action. Remote tethering is
//! a different operation: one certified device asks an exact certified phone
//! to offer connectivity, the phone prompts locally, and an accepted request
//! receives a short-lived nonce plus a Wi-Fi credential sealed only to the
//! requester. Plaintext passwords are not representable in this protocol.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::shell::device_fabric::FabricStamp;
use crate::shell::device_fabric::WifiCredential;
use crate::shell::device_fabric_secrets::wifi_envelope_recipients;
use crate::shell::personal_continuity::DeviceId;

pub const TETHERING_PROTOCOL_VERSION: u16 = 1;
pub const REQUEST_TTL_MS: u64 = 30_000;
pub const GRANT_TTL_MS: u64 = 120_000;
const MAX_PENDING: usize = 16;
const MAX_LABEL_BYTES: usize = 160;

pub type TetheringId = [u8; 16];
pub type TetheringNonce = [u8; 16];

/// The two intentionally different Quick Settings choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TetheringRoute {
    /// Turn this machine's `NetworkManager` hotspot on or off.
    LocalHotspot,
    /// Ask one certified phone to offer tethering. This never toggles the
    /// requester's local hotspot profile.
    CertifiedPhone(DeviceId),
}

/// Ephemeral, certified-device messages. They never enter a fabric snapshot.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TetheringSignal {
    Request {
        protocol_version: u16,
        request_id: TetheringId,
        requester: DeviceId,
        provider: DeviceId,
        requester_label: String,
        issued_unix_ms: u64,
        expires_unix_ms: u64,
    },
    Decision {
        protocol_version: u16,
        request_id: TetheringId,
        requester: DeviceId,
        provider: DeviceId,
        accepted: bool,
        grant_nonce: Option<TetheringNonce>,
        expires_unix_ms: u64,
    },
    /// A credential produced by `seal_wifi_credential` with exactly one
    /// recipient: `requester`. The passphrase exists only at the provider and
    /// briefly after decryption on the requester.
    Credential {
        protocol_version: u16,
        request_id: TetheringId,
        requester: DeviceId,
        provider: DeviceId,
        grant_nonce: TetheringNonce,
        expires_unix_ms: u64,
        credential: WifiCredential,
    },
    Stop {
        protocol_version: u16,
        request_id: TetheringId,
        requester: DeviceId,
        provider: DeviceId,
        grant_nonce: TetheringNonce,
    },
    /// Requester-authored cancellation for a pending or active grant. A
    /// granted request must echo the exact provider nonce.
    Cancel {
        protocol_version: u16,
        request_id: TetheringId,
        requester: DeviceId,
        provider: DeviceId,
        grant_nonce: Option<TetheringNonce>,
    },
}

impl std::fmt::Debug for TetheringSignal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request {
                request_id,
                requester,
                provider,
                ..
            } => formatter
                .debug_struct("Request")
                .field("request_id", request_id)
                .field("requester", requester)
                .field("provider", provider)
                .finish_non_exhaustive(),
            Self::Decision {
                request_id,
                requester,
                provider,
                accepted,
                ..
            } => formatter
                .debug_struct("Decision")
                .field("request_id", request_id)
                .field("requester", requester)
                .field("provider", provider)
                .field("accepted", accepted)
                .finish_non_exhaustive(),
            Self::Credential {
                request_id,
                requester,
                provider,
                ..
            } => formatter
                .debug_struct("Credential")
                .field("request_id", request_id)
                .field("requester", requester)
                .field("provider", provider)
                .field("credential", &"<sealed>")
                .finish(),
            Self::Stop {
                request_id,
                requester,
                provider,
                ..
            } => formatter
                .debug_struct("Stop")
                .field("request_id", request_id)
                .field("requester", requester)
                .field("provider", provider)
                .finish_non_exhaustive(),
            Self::Cancel {
                request_id,
                requester,
                provider,
                ..
            } => formatter
                .debug_struct("Cancel")
                .field("request_id", request_id)
                .field("requester", requester)
                .field("provider", provider)
                .finish_non_exhaustive(),
        }
    }
}

impl TetheringSignal {
    /// Validate the immutable transport-visible contract. Stateful replay and
    /// grant checks remain the responsibility of [`PersonalTethering`].
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::InvalidRequest`], [`TetheringError::InvalidDecision`]
    /// or [`TetheringError::InvalidCredential`], by signal kind, when the signal
    /// breaks its wire contract for this source, target and time.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive validator checks every tethering signal's wire contract"
    )]
    pub fn validate_transport(
        &self,
        authenticated_source: &DeviceId,
        local_target: &DeviceId,
        now_unix_ms: u64,
    ) -> Result<(), TetheringError> {
        match self {
            Self::Request {
                protocol_version,
                request_id,
                requester,
                provider,
                requester_label,
                issued_unix_ms,
                expires_unix_ms,
            } => {
                if *protocol_version != TETHERING_PROTOCOL_VERSION
                    || requester != authenticated_source
                    || provider != local_target
                    || requester == provider
                    || !valid_id(*request_id)
                    || !valid_label(requester_label)
                    || *issued_unix_ms > now_unix_ms
                    || *expires_unix_ms <= now_unix_ms
                    || expires_unix_ms.saturating_sub(*issued_unix_ms) > REQUEST_TTL_MS
                {
                    return Err(TetheringError::InvalidRequest);
                }
            }
            Self::Decision {
                protocol_version,
                request_id,
                requester,
                provider,
                accepted,
                grant_nonce,
                expires_unix_ms,
            } => {
                if *protocol_version != TETHERING_PROTOCOL_VERSION
                    || provider != authenticated_source
                    || requester != local_target
                    || requester == provider
                    || !valid_id(*request_id)
                    || (*accepted
                        && (grant_nonce.is_none_or(|nonce| !valid_id(nonce))
                            || *expires_unix_ms <= now_unix_ms
                            || expires_unix_ms.saturating_sub(now_unix_ms) > GRANT_TTL_MS))
                    || (!*accepted && grant_nonce.is_some())
                {
                    return Err(TetheringError::InvalidDecision);
                }
            }
            Self::Credential {
                protocol_version,
                request_id,
                requester,
                provider,
                grant_nonce,
                expires_unix_ms,
                credential,
            } => {
                if *protocol_version != TETHERING_PROTOCOL_VERSION
                    || provider != authenticated_source
                    || requester != local_target
                    || requester == provider
                    || !valid_id(*request_id)
                    || !valid_id(*grant_nonce)
                    || *expires_unix_ms <= now_unix_ms
                    || expires_unix_ms.saturating_sub(now_unix_ms) > GRANT_TTL_MS
                {
                    return Err(TetheringError::InvalidCredential);
                }
                validate_credential(credential, provider, requester)?;
            }
            Self::Stop {
                protocol_version,
                request_id,
                requester,
                provider,
                grant_nonce,
            } => {
                if *protocol_version != TETHERING_PROTOCOL_VERSION
                    || provider != authenticated_source
                    || requester != local_target
                    || requester == provider
                    || !valid_id(*request_id)
                    || !valid_id(*grant_nonce)
                {
                    return Err(TetheringError::InvalidDecision);
                }
            }
            Self::Cancel {
                protocol_version,
                request_id,
                requester,
                provider,
                grant_nonce,
            } => {
                if *protocol_version != TETHERING_PROTOCOL_VERSION
                    || requester != authenticated_source
                    || provider != local_target
                    || requester == provider
                    || !valid_id(*request_id)
                    || grant_nonce.is_some_and(|nonce| !valid_id(nonce))
                {
                    return Err(TetheringError::InvalidDecision);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TetheringEvent {
    ConsentRequired {
        request_id: TetheringId,
        requester: DeviceId,
        requester_label: String,
        expires_unix_ms: u64,
    },
    Rejected {
        request_id: TetheringId,
        provider: DeviceId,
    },
    /// The shell may decrypt this sealed credential only immediately before
    /// passing it to `NetworkManager`. UI and logs receive the metadata fields.
    ConnectOffered {
        request_id: TetheringId,
        provider: DeviceId,
        expires_unix_ms: u64,
        credential: WifiCredential,
    },
    Ended {
        request_id: TetheringId,
        peer: DeviceId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingInbound {
    requester: DeviceId,
    requester_label: String,
    expires_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Outbound {
    provider: DeviceId,
    expires_unix_ms: u64,
    grant_nonce: Option<TetheringNonce>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProviderLease {
    requester: DeviceId,
    expires_unix_ms: u64,
    grant_nonce: TetheringNonce,
}

#[derive(Default)]
pub struct PersonalTethering {
    inbound: BTreeMap<TetheringId, PendingInbound>,
    outbound: BTreeMap<TetheringId, Outbound>,
    leases: BTreeMap<TetheringId, ProviderLease>,
}

impl PersonalTethering {
    /// Begin a remote request. `LocalHotspot` is rejected because it belongs
    /// to the existing local `NetworkManager` action, not this protocol.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::LocalRoute`] for the local hotspot route and
    /// [`TetheringError::InvalidRequest`] when the requester names itself, the id or
    /// label is invalid, the id is already pending, or too many requests are pending.
    pub fn request(
        &mut self,
        route: TetheringRoute,
        requester: &DeviceId,
        requester_label: &str,
        request_id: TetheringId,
        now_unix_ms: u64,
    ) -> Result<TetheringSignal, TetheringError> {
        let TetheringRoute::CertifiedPhone(provider) = route else {
            return Err(TetheringError::LocalRoute);
        };
        if provider == *requester
            || !valid_id(request_id)
            || !valid_label(requester_label)
            || self.outbound.contains_key(&request_id)
            || self.outbound.len() >= MAX_PENDING
        {
            return Err(TetheringError::InvalidRequest);
        }
        let expires_unix_ms = now_unix_ms.saturating_add(REQUEST_TTL_MS);
        self.outbound.insert(
            request_id,
            Outbound {
                provider: provider.clone(),
                expires_unix_ms,
                grant_nonce: None,
            },
        );
        Ok(TetheringSignal::Request {
            protocol_version: TETHERING_PROTOCOL_VERSION,
            request_id,
            requester: requester.clone(),
            provider,
            requester_label: requester_label.to_string(),
            issued_unix_ms: now_unix_ms,
            expires_unix_ms,
        })
    }

    /// Accept an authenticated request only when its claimed requester is the
    /// transport source and its target is this exact phone. Duplicate ids are
    /// rejected rather than replacing another pending consent.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::UnexpectedSignal`] for any other signal kind and
    /// [`TetheringError::InvalidRequest`] for an unsupported version, a request not
    /// from `authenticated_source` to `local_device`, an invalid id or label, a stale
    /// or expired request, a duplicate id, or too many pending consents.
    pub fn receive_request(
        &mut self,
        authenticated_source: &DeviceId,
        local_device: &DeviceId,
        signal: &TetheringSignal,
        now_unix_ms: u64,
    ) -> Result<TetheringEvent, TetheringError> {
        let TetheringSignal::Request {
            protocol_version,
            request_id,
            requester,
            provider,
            requester_label,
            issued_unix_ms,
            expires_unix_ms,
        } = signal
        else {
            return Err(TetheringError::UnexpectedSignal);
        };
        if *protocol_version != TETHERING_PROTOCOL_VERSION
            || requester != authenticated_source
            || provider != local_device
            || requester == provider
            || !valid_id(*request_id)
            || !valid_label(requester_label)
            || *issued_unix_ms > now_unix_ms
            || *expires_unix_ms <= now_unix_ms
            || expires_unix_ms.saturating_sub(*issued_unix_ms) > REQUEST_TTL_MS
            || self.inbound.contains_key(request_id)
            || self.inbound.len() >= MAX_PENDING
        {
            return Err(TetheringError::InvalidRequest);
        }
        self.inbound.insert(
            *request_id,
            PendingInbound {
                requester: requester.clone(),
                requester_label: requester_label.clone(),
                expires_unix_ms: *expires_unix_ms,
            },
        );
        Ok(TetheringEvent::ConsentRequired {
            request_id: *request_id,
            requester: requester.clone(),
            requester_label: requester_label.clone(),
            expires_unix_ms: *expires_unix_ms,
        })
    }

    /// Apply the phone user's explicit decision. Acceptance requires a fresh
    /// nonzero nonce and a credential sealed only to the exact requester.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::UnknownRequest`] for an id with no pending
    /// consent, [`TetheringError::Expired`] once it lapsed, and
    /// [`TetheringError::InvalidDecision`] or [`TetheringError::InvalidCredential`]
    /// when the nonce or credential does not fit the decision.
    pub fn decide(
        &mut self,
        local_device: &DeviceId,
        request_id: TetheringId,
        accepted: bool,
        grant_nonce: Option<TetheringNonce>,
        credential: Option<WifiCredential>,
        now_unix_ms: u64,
    ) -> Result<Vec<TetheringSignal>, TetheringError> {
        let pending = self
            .inbound
            .remove(&request_id)
            .ok_or(TetheringError::UnknownRequest)?;
        if pending.expires_unix_ms <= now_unix_ms {
            return Err(TetheringError::Expired);
        }
        if !accepted {
            if grant_nonce.is_some() || credential.is_some() {
                return Err(TetheringError::InvalidDecision);
            }
            return Ok(vec![TetheringSignal::Decision {
                protocol_version: TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: pending.requester,
                provider: local_device.clone(),
                accepted: false,
                grant_nonce: None,
                expires_unix_ms: now_unix_ms,
            }]);
        }
        let nonce = grant_nonce
            .filter(|nonce| valid_id(*nonce))
            .ok_or(TetheringError::InvalidDecision)?;
        let credential = credential.ok_or(TetheringError::InvalidDecision)?;
        validate_credential(&credential, local_device, &pending.requester)?;
        let expires_unix_ms = now_unix_ms.saturating_add(GRANT_TTL_MS);
        self.leases.insert(
            request_id,
            ProviderLease {
                requester: pending.requester.clone(),
                expires_unix_ms,
                grant_nonce: nonce,
            },
        );
        Ok(vec![
            TetheringSignal::Decision {
                protocol_version: TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: pending.requester.clone(),
                provider: local_device.clone(),
                accepted: true,
                grant_nonce: Some(nonce),
                expires_unix_ms,
            },
            TetheringSignal::Credential {
                protocol_version: TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: pending.requester,
                provider: local_device.clone(),
                grant_nonce: nonce,
                expires_unix_ms,
                credential,
            },
        ])
    }

    /// Apply the provider's authenticated decision to this device's request.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::UnexpectedSignal`] for any other signal kind,
    /// [`TetheringError::UnknownRequest`] for an id this device did not request, and
    /// [`TetheringError::InvalidDecision`] for an unsupported version, another route,
    /// an expired request, a nonce that does not fit the decision, or a grant expiry
    /// outside its bounds.
    pub fn receive_decision(
        &mut self,
        authenticated_source: &DeviceId,
        local_device: &DeviceId,
        signal: &TetheringSignal,
        now_unix_ms: u64,
    ) -> Result<Option<TetheringEvent>, TetheringError> {
        let TetheringSignal::Decision {
            protocol_version,
            request_id,
            requester,
            provider,
            accepted,
            grant_nonce,
            expires_unix_ms,
        } = signal
        else {
            return Err(TetheringError::UnexpectedSignal);
        };
        let outbound = self
            .outbound
            .get_mut(request_id)
            .ok_or(TetheringError::UnknownRequest)?;
        if *protocol_version != TETHERING_PROTOCOL_VERSION
            || provider != authenticated_source
            || requester != local_device
            || provider != &outbound.provider
            || outbound.expires_unix_ms <= now_unix_ms
        {
            return Err(TetheringError::InvalidDecision);
        }
        if !accepted {
            if grant_nonce.is_some() {
                return Err(TetheringError::InvalidDecision);
            }
            self.outbound.remove(request_id);
            return Ok(Some(TetheringEvent::Rejected {
                request_id: *request_id,
                provider: provider.clone(),
            }));
        }
        let nonce = grant_nonce
            .filter(|nonce| valid_id(*nonce))
            .ok_or(TetheringError::InvalidDecision)?;
        if *expires_unix_ms <= now_unix_ms
            || expires_unix_ms.saturating_sub(now_unix_ms) > GRANT_TTL_MS
        {
            return Err(TetheringError::InvalidDecision);
        }
        outbound.grant_nonce = Some(nonce);
        outbound.expires_unix_ms = *expires_unix_ms;
        Ok(None)
    }

    /// Accept the sealed credential for a granted request.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::UnexpectedSignal`] for any other signal kind,
    /// [`TetheringError::UnknownRequest`] for an id this device did not request, and
    /// [`TetheringError::InvalidCredential`] for an unsupported version, another
    /// route, a nonce or expiry that differs from the grant, an expired grant, or a
    /// credential not sealed to this requester.
    pub fn receive_credential(
        &mut self,
        authenticated_source: &DeviceId,
        local_device: &DeviceId,
        signal: &TetheringSignal,
        now_unix_ms: u64,
    ) -> Result<TetheringEvent, TetheringError> {
        let TetheringSignal::Credential {
            protocol_version,
            request_id,
            requester,
            provider,
            grant_nonce,
            expires_unix_ms,
            credential,
        } = signal
        else {
            return Err(TetheringError::UnexpectedSignal);
        };
        let outbound = self
            .outbound
            .get(request_id)
            .ok_or(TetheringError::UnknownRequest)?;
        if *protocol_version != TETHERING_PROTOCOL_VERSION
            || provider != authenticated_source
            || requester != local_device
            || provider != &outbound.provider
            || outbound.grant_nonce != Some(*grant_nonce)
            || outbound.expires_unix_ms != *expires_unix_ms
            || *expires_unix_ms <= now_unix_ms
        {
            return Err(TetheringError::InvalidCredential);
        }
        validate_credential(credential, provider, requester)?;
        Ok(TetheringEvent::ConnectOffered {
            request_id: *request_id,
            provider: provider.clone(),
            expires_unix_ms: *expires_unix_ms,
            credential: credential.clone(),
        })
    }

    /// End this device's request when its provider stops the lease.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::UnexpectedSignal`] for any other signal kind,
    /// [`TetheringError::UnknownRequest`] for an id this device did not request, and
    /// [`TetheringError::InvalidDecision`] for an unsupported version, another route,
    /// or a nonce that differs from the grant.
    pub fn receive_stop(
        &mut self,
        authenticated_source: &DeviceId,
        local_device: &DeviceId,
        signal: &TetheringSignal,
    ) -> Result<TetheringEvent, TetheringError> {
        let TetheringSignal::Stop {
            protocol_version,
            request_id,
            requester,
            provider,
            grant_nonce,
        } = signal
        else {
            return Err(TetheringError::UnexpectedSignal);
        };
        let outbound = self
            .outbound
            .get(request_id)
            .ok_or(TetheringError::UnknownRequest)?;
        if *protocol_version != TETHERING_PROTOCOL_VERSION
            || provider != authenticated_source
            || requester != local_device
            || provider != &outbound.provider
            || outbound.grant_nonce != Some(*grant_nonce)
        {
            return Err(TetheringError::InvalidDecision);
        }
        self.outbound.remove(request_id);
        Ok(TetheringEvent::Ended {
            request_id: *request_id,
            peer: provider.clone(),
        })
    }

    /// Cancel a requester-owned pending or active request and return the exact
    /// authenticated cancellation message for its provider.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::UnknownRequest`] when this device has no such
    /// request.
    pub fn cancel_request(
        &mut self,
        local_device: &DeviceId,
        request_id: TetheringId,
    ) -> Result<TetheringSignal, TetheringError> {
        let outbound = self
            .outbound
            .remove(&request_id)
            .ok_or(TetheringError::UnknownRequest)?;
        Ok(TetheringSignal::Cancel {
            protocol_version: TETHERING_PROTOCOL_VERSION,
            request_id,
            requester: local_device.clone(),
            provider: outbound.provider,
            grant_nonce: outbound.grant_nonce,
        })
    }

    /// Consume a requester-authored cancellation only for the exact pending
    /// consent or provider lease and nonce it names.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::UnexpectedSignal`] for any other signal kind,
    /// [`TetheringError::InvalidDecision`] for an unsupported version, another route,
    /// or a requester or nonce that does not match, and
    /// [`TetheringError::UnknownRequest`] when nothing is pending or leased.
    pub fn receive_cancel(
        &mut self,
        authenticated_source: &DeviceId,
        local_device: &DeviceId,
        signal: &TetheringSignal,
    ) -> Result<TetheringEvent, TetheringError> {
        let TetheringSignal::Cancel {
            protocol_version,
            request_id,
            requester,
            provider,
            grant_nonce,
        } = signal
        else {
            return Err(TetheringError::UnexpectedSignal);
        };
        if *protocol_version != TETHERING_PROTOCOL_VERSION
            || requester != authenticated_source
            || provider != local_device
        {
            return Err(TetheringError::InvalidDecision);
        }
        let removed = if let Some(pending) = self.inbound.get(request_id) {
            if pending.requester != *requester || grant_nonce.is_some() {
                return Err(TetheringError::InvalidDecision);
            }
            self.inbound.remove(request_id);
            true
        } else if let Some(lease) = self.leases.get(request_id) {
            if lease.requester != *requester || *grant_nonce != Some(lease.grant_nonce) {
                return Err(TetheringError::InvalidDecision);
            }
            self.leases.remove(request_id);
            true
        } else {
            false
        };
        if !removed {
            return Err(TetheringError::UnknownRequest);
        }
        Ok(TetheringEvent::Ended {
            request_id: *request_id,
            peer: requester.clone(),
        })
    }

    /// End a provider-owned active lease and produce its exact stop message.
    ///
    /// # Errors
    ///
    /// Returns [`TetheringError::UnknownRequest`] when this device holds no such
    /// lease.
    pub fn stop_provider_lease(
        &mut self,
        local_device: &DeviceId,
        request_id: TetheringId,
    ) -> Result<TetheringSignal, TetheringError> {
        let lease = self
            .leases
            .remove(&request_id)
            .ok_or(TetheringError::UnknownRequest)?;
        Ok(TetheringSignal::Stop {
            protocol_version: TETHERING_PROTOCOL_VERSION,
            request_id,
            requester: lease.requester,
            provider: local_device.clone(),
            grant_nonce: lease.grant_nonce,
        })
    }

    /// Tear down every request and lease involving `peer`. Provider leases
    /// produce an exact stop message; pending consents and requester state are
    /// simply forgotten. Revocation and disconnect call this same path.
    pub fn peer_unavailable(
        &mut self,
        local_device: &DeviceId,
        peer: &DeviceId,
    ) -> Vec<TetheringSignal> {
        self.inbound.retain(|_, pending| &pending.requester != peer);
        self.outbound
            .retain(|_, outbound| &outbound.provider != peer);
        let ended = self
            .leases
            .iter()
            .filter(|(_, lease)| &lease.requester == peer)
            .map(|(request_id, lease)| (*request_id, lease.clone()))
            .collect::<Vec<_>>();
        for (request_id, _) in &ended {
            self.leases.remove(request_id);
        }
        ended
            .into_iter()
            .map(|(request_id, lease)| TetheringSignal::Stop {
                protocol_version: TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: lease.requester,
                provider: local_device.clone(),
                grant_nonce: lease.grant_nonce,
            })
            .collect()
    }

    /// Device-local privacy revocation tears down all active grants and
    /// discards every pending request.
    pub fn policy_disabled(&mut self, local_device: &DeviceId) -> Vec<TetheringSignal> {
        self.inbound.clear();
        self.outbound.clear();
        let leases = std::mem::take(&mut self.leases);
        leases
            .into_iter()
            .map(|(request_id, lease)| TetheringSignal::Stop {
                protocol_version: TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: lease.requester,
                provider: local_device.clone(),
                grant_nonce: lease.grant_nonce,
            })
            .collect()
    }

    pub fn expire(&mut self, local_device: &DeviceId, now_unix_ms: u64) -> Vec<TetheringSignal> {
        self.inbound
            .retain(|_, pending| pending.expires_unix_ms > now_unix_ms);
        self.outbound
            .retain(|_, outbound| outbound.expires_unix_ms > now_unix_ms);
        let expired = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.expires_unix_ms <= now_unix_ms)
            .map(|(request_id, lease)| (*request_id, lease.clone()))
            .collect::<Vec<_>>();
        for (request_id, _) in &expired {
            self.leases.remove(request_id);
        }
        expired
            .into_iter()
            .map(|(request_id, lease)| TetheringSignal::Stop {
                protocol_version: TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: lease.requester,
                provider: local_device.clone(),
                grant_nonce: lease.grant_nonce,
            })
            .collect()
    }
}

fn validate_credential(
    credential: &WifiCredential,
    provider: &DeviceId,
    requester: &DeviceId,
) -> Result<(), TetheringError> {
    if credential.stamp.device != *provider || !credential.security.supports_portable_passphrase() {
        return Err(TetheringError::InvalidCredential);
    }
    let recipients =
        wifi_envelope_recipients(credential).map_err(|_| TetheringError::InvalidCredential)?;
    if recipients != BTreeSet::from([requester.clone()]) {
        return Err(TetheringError::InvalidCredential);
    }
    Ok(())
}

fn valid_id(id: [u8; 16]) -> bool {
    id != [0; 16]
}

fn valid_label(label: &str) -> bool {
    !label.is_empty() && label.len() <= MAX_LABEL_BYTES && !label.chars().any(char::is_control)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TetheringError {
    LocalRoute,
    InvalidRequest,
    UnknownRequest,
    Expired,
    InvalidDecision,
    InvalidCredential,
    UnexpectedSignal,
}

impl std::fmt::Display for TetheringError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::LocalRoute => "local hotspot requests do not use personal tethering",
            Self::InvalidRequest => "invalid personal tethering request",
            Self::UnknownRequest => "unknown personal tethering request",
            Self::Expired => "personal tethering request expired",
            Self::InvalidDecision => "invalid personal tethering decision",
            Self::InvalidCredential => "invalid sealed tethering credential",
            Self::UnexpectedSignal => "unexpected personal tethering signal",
        })
    }
}

impl std::error::Error for TetheringError {}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn devices() -> (SigningKey, DeviceId, SigningKey, DeviceId) {
        let requester_key = SigningKey::from_bytes(&[3; 32]);
        let provider_key = SigningKey::from_bytes(&[4; 32]);
        let requester =
            DeviceId::new(hex::encode(requester_key.verifying_key().to_bytes())).unwrap();
        let provider = DeviceId::new(hex::encode(provider_key.verifying_key().to_bytes())).unwrap();
        (requester_key, requester, provider_key, provider)
    }

    fn sealed(
        requester: &DeviceId,
        provider: &DeviceId,
        password: &str,
        counter: u64,
    ) -> WifiCredential {
        crate::shell::device_fabric_secrets::seal_wifi_credential(
            "Phone hotspot",
            os_actions::WifiSecurity::Wpa3,
            password,
            provider,
            [requester.clone()],
            FabricStamp {
                counter,
                device: provider.clone(),
            },
        )
        .unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tethering_requests_are_exact_consent_bound_and_spoof_resistant() {
        fn local_hotspot_is_not_a_remote_tethering_request() {
            let (_, requester, _, _) = devices();
            assert_eq!(
                PersonalTethering::default().request(
                    TetheringRoute::LocalHotspot,
                    &requester,
                    "Laptop",
                    [1; 16],
                    100,
                ),
                Err(TetheringError::LocalRoute)
            );
        }

        fn accepted_request_is_exact_consent_bound_and_secret_free_on_the_wire() {
            let (requester_key, requester, _, provider) = devices();
            let mut requester_state = PersonalTethering::default();
            let mut provider_state = PersonalTethering::default();
            let request = requester_state
                .request(
                    TetheringRoute::CertifiedPhone(provider.clone()),
                    &requester,
                    "Laptop",
                    [1; 16],
                    1_000,
                )
                .unwrap();
            assert!(matches!(
                provider_state.receive_request(&requester, &provider, &request, 1_001),
                Ok(TetheringEvent::ConsentRequired { .. })
            ));
            let credential = sealed(&requester, &provider, "not-in-wire-debug", 2);
            let signals = provider_state
                .decide(
                    &provider,
                    [1; 16],
                    true,
                    Some([9; 16]),
                    Some(credential),
                    1_002,
                )
                .unwrap();
            assert_eq!(signals.len(), 2);
            requester_state
                .receive_decision(&provider, &requester, &signals[0], 1_003)
                .unwrap();
            let event = requester_state
                .receive_credential(&provider, &requester, &signals[1], 1_004)
                .unwrap();
            let TetheringEvent::ConnectOffered { credential, .. } = event else {
                panic!("expected connect offer");
            };
            assert_eq!(
                crate::shell::device_fabric_secrets::open_wifi_credential(
                    &credential,
                    &requester_key
                )
                .unwrap(),
                "not-in-wire-debug"
            );
            assert!(!format!("{:?}", signals[1]).contains("not-in-wire-debug"));
            let encoded = postcard::to_allocvec(&signals[1]).unwrap();
            assert!(
                !encoded
                    .windows("not-in-wire-debug".len())
                    .any(|window| window == b"not-in-wire-debug")
            );
        }

        fn spoof_replay_and_wrong_recipient_fail_closed() {
            let (_, requester, _, provider) = devices();
            let attacker_key = SigningKey::from_bytes(&[5; 32]);
            let attacker =
                DeviceId::new(hex::encode(attacker_key.verifying_key().to_bytes())).unwrap();
            let mut requester_state = PersonalTethering::default();
            let request = requester_state
                .request(
                    TetheringRoute::CertifiedPhone(provider.clone()),
                    &requester,
                    "Laptop",
                    [2; 16],
                    2_000,
                )
                .unwrap();
            let mut provider_state = PersonalTethering::default();
            assert_eq!(
                provider_state.receive_request(&attacker, &provider, &request, 2_001),
                Err(TetheringError::InvalidRequest)
            );
            provider_state
                .receive_request(&requester, &provider, &request, 2_001)
                .unwrap();
            assert_eq!(
                provider_state.receive_request(&requester, &provider, &request, 2_001),
                Err(TetheringError::InvalidRequest)
            );
            let wrong = sealed(&attacker, &provider, "private-pass", 3);
            assert_eq!(
                provider_state.decide(&provider, [2; 16], true, Some([8; 16]), Some(wrong), 2_002,),
                Err(TetheringError::InvalidCredential)
            );
        }

        local_hotspot_is_not_a_remote_tethering_request();
        accepted_request_is_exact_consent_bound_and_secret_free_on_the_wire();
        spoof_replay_and_wrong_recipient_fail_closed();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tethering_leases_tear_down_exactly_on_revocation_expiry_or_nonce_cancel() {
        fn disconnect_revocation_policy_and_expiry_tear_down_exact_leases() {
            let (_, requester, _, provider) = devices();
            let mut requester_state = PersonalTethering::default();
            let mut provider_state = PersonalTethering::default();
            for (id, now) in [([3; 16], 3_000), ([4; 16], 4_000), ([5; 16], 5_000)] {
                let request = requester_state
                    .request(
                        TetheringRoute::CertifiedPhone(provider.clone()),
                        &requester,
                        "Laptop",
                        id,
                        now,
                    )
                    .unwrap();
                provider_state
                    .receive_request(&requester, &provider, &request, now + 1)
                    .unwrap();
                provider_state
                    .decide(
                        &provider,
                        id,
                        true,
                        Some(id),
                        Some(sealed(&requester, &provider, "private-pass", now)),
                        now + 2,
                    )
                    .unwrap();
            }
            assert_eq!(
                provider_state.peer_unavailable(&provider, &requester).len(),
                3
            );
            assert!(provider_state.policy_disabled(&provider).is_empty());

            let request = requester_state
                .request(
                    TetheringRoute::CertifiedPhone(provider.clone()),
                    &requester,
                    "Laptop",
                    [6; 16],
                    6_000,
                )
                .unwrap();
            provider_state
                .receive_request(&requester, &provider, &request, 6_001)
                .unwrap();
            provider_state
                .decide(
                    &provider,
                    [6; 16],
                    true,
                    Some([6; 16]),
                    Some(sealed(&requester, &provider, "private-pass", 6)),
                    6_002,
                )
                .unwrap();
            assert_eq!(
                provider_state.expire(&provider, 6_002 + GRANT_TTL_MS).len(),
                1
            );
        }

        fn requester_cancel_requires_the_exact_grant_nonce() {
            let (_, requester, _, provider) = devices();
            let mut provider_state = PersonalTethering::default();
            let mut requester_state = PersonalTethering::default();
            let request_id = [21; 16];
            let request = requester_state
                .request(
                    TetheringRoute::CertifiedPhone(provider.clone()),
                    &requester,
                    "Laptop",
                    request_id,
                    1_000,
                )
                .unwrap();
            provider_state
                .receive_request(&requester, &provider, &request, 1_000)
                .unwrap();
            let signals = provider_state
                .decide(
                    &provider,
                    request_id,
                    true,
                    Some([22; 16]),
                    Some(sealed(&requester, &provider, "secret-passphrase", 1)),
                    1_001,
                )
                .unwrap();
            requester_state
                .receive_decision(&provider, &requester, &signals[0], 1_001)
                .unwrap();
            requester_state
                .receive_credential(&provider, &requester, &signals[1], 1_001)
                .unwrap();

            let cancel = requester_state
                .cancel_request(&requester, request_id)
                .unwrap();
            let mut spoof = cancel.clone();
            if let TetheringSignal::Cancel { grant_nonce, .. } = &mut spoof {
                *grant_nonce = Some([23; 16]);
            }
            assert_eq!(
                provider_state.receive_cancel(&requester, &provider, &spoof),
                Err(TetheringError::InvalidDecision)
            );
            assert!(
                provider_state
                    .receive_cancel(&requester, &provider, &cancel)
                    .is_ok()
            );
            assert_eq!(
                provider_state.stop_provider_lease(&provider, request_id),
                Err(TetheringError::UnknownRequest)
            );
        }

        disconnect_revocation_policy_and_expiry_tear_down_exact_leases();
        requester_cancel_requires_the_exact_grant_nonce();
    }
}
