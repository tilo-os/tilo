//! Target-local capture source broker.
//!
//! This module deliberately stops at the privacy boundary: verified app,
//! workspace and session identities may request short-lived leases, while raw
//! PipeWire node IDs and file descriptors remain process-local and are never
//! serializable. A future cross-device media plane can consume a local lease;
//! it must not copy these handles into continuity state.

use std::collections::{BTreeMap, VecDeque};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::shell::realtime_session::VerifiedAppRegistration;

const MAX_SOURCES: usize = 128;
const MAX_AUTHORIZATIONS: usize = 256;
const MAX_LEASES: usize = 256;
const MAX_PRIVACY_EVENTS: usize = MAX_LEASES * 4;
pub const CAPTURE_AUTHORIZATION_TTL: Duration = Duration::from_secs(30);
pub const CAPTURE_LEASE_TTL: Duration = Duration::from_secs(5 * 60);
const OPAQUE_ID_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CaptureKind {
    Camera,
    Microphone,
    Screen,
    SystemAudio,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureSessionBinding {
    app_id: String,
    workspace_id: String,
    session_id: String,
    target_id: String,
}

impl CaptureSessionBinding {
    /// Bind a local capture session to a shell-verified app registration.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, or control-bearing session ID.
    pub fn from_verified(
        app: &VerifiedAppRegistration,
        session_id: impl Into<String>,
        target_id: impl Into<String>,
    ) -> Result<Self, CaptureBrokerError> {
        let session_id = session_id.into();
        let target_id = target_id.into();
        if !valid_identifier(&session_id, 256) || !valid_identifier(&target_id, 256) {
            return Err(CaptureBrokerError::InvalidIdentity);
        }
        Ok(Self {
            app_id: app.app_id().to_string(),
            workspace_id: app.workspace_id().to_string(),
            session_id,
            target_id,
        })
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    #[must_use]
    pub fn target_id(&self) -> &str {
        &self.target_id
    }
}

/// A local backend capability. It intentionally has no serialization traits.
#[derive(Debug)]
pub enum LocalCaptureHandle {
    PipeWireNode(u32),
    FileDescriptor(OwnedFd),
    /// Shell-selected system default. The concrete device remains behind the
    /// portal/media backend and may change without invalidating app state.
    SystemDefault(CaptureKind),
}

#[derive(Clone, Copy, Debug)]
pub enum BorrowedCaptureHandle<'a> {
    PipeWireNode(u32),
    FileDescriptor(BorrowedFd<'a>),
    SystemDefault(CaptureKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureSourceView {
    pub id: String,
    pub label: String,
    pub kind: CaptureKind,
}

struct LocalCaptureSource {
    view: CaptureSourceView,
    handle: LocalCaptureHandle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaptureAuthorizationId {
    broker: [u8; 16],
    nonce: [u8; 16],
}

impl CaptureAuthorizationId {
    /// Encode this target-local capability for the verified local app IPC.
    /// The value is deliberately opaque and restart scoped; it is not a
    /// continuation or device-fabric identifier.
    #[must_use]
    pub fn to_opaque(self) -> String {
        let mut bytes = [0_u8; OPAQUE_ID_BYTES];
        bytes[..16].copy_from_slice(&self.broker);
        bytes[16..].copy_from_slice(&self.nonce);
        hex::encode(bytes)
    }

    /// Decode an opaque target-local authorization capability.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is the exact fixed-width lowercase
    /// hexadecimal representation emitted by [`Self::to_opaque`].
    pub fn from_opaque(value: &str) -> Result<Self, CaptureBrokerError> {
        if value.len() != OPAQUE_ID_BYTES.saturating_mul(2)
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CaptureBrokerError::InvalidAuthorization);
        }
        let bytes = hex::decode(value).map_err(|_| CaptureBrokerError::InvalidAuthorization)?;
        let (broker, nonce) = bytes.split_at(16);
        Ok(Self {
            broker: broker
                .try_into()
                .map_err(|_| CaptureBrokerError::InvalidAuthorization)?,
            nonce: nonce
                .try_into()
                .map_err(|_| CaptureBrokerError::InvalidAuthorization)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaptureLeaseId {
    broker: [u8; 16],
    nonce: [u8; 16],
}

/// A target-local lease and its independently authorized media kind. Lease
/// IDs remain shell-owned and are never serialized into continuation state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureLeaseGrant {
    pub kind: CaptureKind,
    pub lease_id: CaptureLeaseId,
}

struct Authorization {
    binding: CaptureSessionBinding,
    source_id: String,
    kind: CaptureKind,
    expires_at: Instant,
}

struct Lease {
    binding: CaptureSessionBinding,
    source_id: String,
    kind: CaptureKind,
    expires_at: Instant,
    stop: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureRevocation {
    Released,
    SessionEnded,
    AppExited,
    ScreenLocked,
    DeviceLost,
    KillSwitch,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapturePrivacyEvent {
    Started {
        lease_id: CaptureLeaseId,
        app_id: String,
        workspace_id: String,
        session_id: String,
        source_id: String,
        kind: CaptureKind,
    },
    Stopped {
        lease_id: CaptureLeaseId,
        app_id: String,
        workspace_id: String,
        session_id: String,
        source_id: String,
        kind: CaptureKind,
        reason: CaptureRevocation,
    },
}

pub struct CaptureBroker {
    instance: [u8; 16],
    sources: BTreeMap<String, LocalCaptureSource>,
    authorizations: BTreeMap<CaptureAuthorizationId, Authorization>,
    leases: BTreeMap<CaptureLeaseId, Lease>,
    disabled: BTreeMap<CaptureKind, bool>,
    events: VecDeque<CapturePrivacyEvent>,
}

impl Default for CaptureBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBroker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            instance: crate::random::nonzero::<16>(),
            sources: BTreeMap::new(),
            authorizations: BTreeMap::new(),
            leases: BTreeMap::new(),
            disabled: BTreeMap::new(),
            events: VecDeque::new(),
        }
    }

    /// Broker seeded with the compositor-owned semantic defaults used by
    /// realtime handoff. These are selectors, not device handles: applications
    /// continue opening media only through their existing sandboxed backend.
    #[must_use]
    pub fn with_system_defaults() -> Self {
        let mut broker = Self::new();
        use crate::shell::i18n::t;
        for (id, label, kind) in [
            (
                "default-microphone",
                t("portal-capture-default-microphone"),
                CaptureKind::Microphone,
            ),
            (
                "default-camera",
                t("portal-capture-default-camera"),
                CaptureKind::Camera,
            ),
            (
                "primary-display",
                t("portal-capture-primary-display"),
                CaptureKind::Screen,
            ),
            (
                "system-audio",
                t("portal-capture-system-audio"),
                CaptureKind::SystemAudio,
            ),
        ] {
            let _ =
                broker.register_source(id, label, kind, LocalCaptureHandle::SystemDefault(kind));
        }
        broker
    }

    /// Register or replace one target-local capture source.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid metadata or when the bounded source table is full.
    pub fn register_source(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        kind: CaptureKind,
        handle: LocalCaptureHandle,
    ) -> Result<(), CaptureBrokerError> {
        let id = id.into();
        let label = label.into();
        if !valid_identifier(&id, 256)
            || label.is_empty()
            || label.len() > 256
            || label.chars().any(char::is_control)
        {
            return Err(CaptureBrokerError::InvalidSource);
        }
        if self.sources.len() >= MAX_SOURCES && !self.sources.contains_key(&id) {
            return Err(CaptureBrokerError::Capacity);
        }
        if self.sources.contains_key(&id) {
            self.authorizations
                .retain(|_, authorization| authorization.source_id != id);
            self.revoke_matching(CaptureRevocation::DeviceLost, |lease| lease.source_id == id);
        }
        self.sources.insert(
            id.clone(),
            LocalCaptureSource {
                view: CaptureSourceView { id, label, kind },
                handle,
            },
        );
        Ok(())
    }

    #[must_use]
    pub fn enumerate(&self, kind: CaptureKind) -> Vec<CaptureSourceView> {
        if self.is_disabled(kind) {
            return Vec::new();
        }
        self.sources
            .values()
            .filter(|source| source.view.kind == kind)
            .map(|source| source.view.clone())
            .collect()
    }

    /// Observed local nodes only; symbolic defaults and unpaired descriptors
    /// are not concrete native microphone inputs.
    pub(crate) fn enumerate_local_nodes(&self, kind: CaptureKind) -> Vec<CaptureSourceView> {
        if self.is_disabled(kind) {
            return Vec::new();
        }
        self.sources
            .values()
            .filter(|source| {
                source.view.kind == kind
                    && matches!(source.handle, LocalCaptureHandle::PipeWireNode(_))
            })
            .map(|source| source.view.clone())
            .collect()
    }

    /// Mint one short-lived, one-shot proof of fresh target-local consent.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing/mismatched source, active kill switch, or capacity limit.
    pub fn authorize(
        &mut self,
        binding: &CaptureSessionBinding,
        source_id: &str,
        kind: CaptureKind,
        now: Instant,
    ) -> Result<CaptureAuthorizationId, CaptureBrokerError> {
        self.reap_expired(now);
        self.require_source(source_id, kind)?;
        if self.authorizations.len() >= MAX_AUTHORIZATIONS {
            return Err(CaptureBrokerError::Capacity);
        }
        let id = CaptureAuthorizationId {
            broker: self.instance,
            nonce: self.new_nonce(),
        };
        self.authorizations.insert(
            id,
            Authorization {
                binding: binding.clone(),
                source_id: source_id.to_string(),
                kind,
                expires_at: now + CAPTURE_AUTHORIZATION_TTL,
            },
        );
        Ok(id)
    }

    /// Atomically mint one authorization for every selected capture facet.
    /// Validation and capacity checks complete before any capability exists.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate/missing sources, a kill switch, or if
    /// the complete authorization set would exceed the bounded table.
    pub fn authorize_batch(
        &mut self,
        binding: &CaptureSessionBinding,
        selections: &[(String, CaptureKind)],
        now: Instant,
    ) -> Result<Vec<CaptureAuthorizationId>, CaptureBrokerError> {
        self.reap_expired(now);
        if selections.is_empty()
            || selections.len() > 4
            || self.authorizations.len().saturating_add(selections.len()) > MAX_AUTHORIZATIONS
        {
            return Err(CaptureBrokerError::Capacity);
        }
        for (index, (source_id, kind)) in selections.iter().enumerate() {
            self.require_source(source_id, *kind)?;
            if selections[..index]
                .iter()
                .any(|(_, prior_kind)| prior_kind == kind)
            {
                return Err(CaptureBrokerError::IncompleteAuthorizationSet);
            }
        }
        let mut ids = Vec::with_capacity(selections.len());
        for (source_id, kind) in selections {
            let id = CaptureAuthorizationId {
                broker: self.instance,
                nonce: self.new_nonce(),
            };
            self.authorizations.insert(
                id,
                Authorization {
                    binding: binding.clone(),
                    source_id: source_id.clone(),
                    kind: *kind,
                    expires_at: now + CAPTURE_AUTHORIZATION_TTL,
                },
            );
            ids.push(id);
        }
        Ok(ids)
    }

    /// Consume fresh authorization and create a target-local capture lease.
    ///
    /// # Errors
    ///
    /// Returns an error for replay, expiry, identity/source mismatch, device loss,
    /// kill-switch policy, or capacity exhaustion.
    pub fn acquire(
        &mut self,
        authorization_id: CaptureAuthorizationId,
        binding: &CaptureSessionBinding,
        source_id: &str,
        kind: CaptureKind,
        now: Instant,
    ) -> Result<CaptureLeaseId, CaptureBrokerError> {
        self.reap_expired(now);
        if authorization_id.broker != self.instance {
            return Err(CaptureBrokerError::InvalidAuthorization);
        }
        let authorization = self
            .authorizations
            .get(&authorization_id)
            .ok_or(CaptureBrokerError::InvalidAuthorization)?;
        if authorization.expires_at < now
            || &authorization.binding != binding
            || authorization.source_id != source_id
            || authorization.kind != kind
        {
            return Err(CaptureBrokerError::BindingMismatch);
        }
        self.require_source(source_id, kind)?;
        if self.leases.len() >= MAX_LEASES {
            return Err(CaptureBrokerError::Capacity);
        }
        self.authorizations.remove(&authorization_id);
        let id = CaptureLeaseId {
            broker: self.instance,
            nonce: self.new_nonce(),
        };
        self.leases.insert(
            id,
            Lease {
                stop: None,
                binding: binding.clone(),
                source_id: source_id.to_string(),
                kind,
                expires_at: now + CAPTURE_LEASE_TTL,
            },
        );
        self.queue_event(CapturePrivacyEvent::Started {
            lease_id: id,
            app_id: binding.app_id.clone(),
            workspace_id: binding.workspace_id.clone(),
            session_id: binding.session_id.clone(),
            source_id: source_id.to_string(),
            kind,
        });
        Ok(id)
    }

    /// Atomically consume one authorization for every required capture kind.
    /// Source identifiers are taken from the broker-owned authorizations, not
    /// from application input. Validation completes before any proof is
    /// consumed, so partial failure cannot leave an accidental live lease.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, duplicate, expired, foreign, or
    /// incorrectly bound authorizations, unavailable sources, or capacity.
    pub fn acquire_required(
        &mut self,
        authorization_ids: &[CaptureAuthorizationId],
        binding: &CaptureSessionBinding,
        required: &[CaptureKind],
        now: Instant,
    ) -> Result<Vec<CaptureLeaseGrant>, CaptureBrokerError> {
        self.reap_expired(now);
        if required.is_empty()
            || required.len() > 4
            || authorization_ids.len() != required.len()
            || self.leases.len().saturating_add(required.len()) > MAX_LEASES
        {
            return Err(CaptureBrokerError::IncompleteAuthorizationSet);
        }
        let mut unique_required = required.to_vec();
        unique_required.sort_unstable();
        unique_required.dedup();
        if unique_required.len() != required.len() {
            return Err(CaptureBrokerError::IncompleteAuthorizationSet);
        }

        let mut validated = Vec::with_capacity(required.len());
        for id in authorization_ids {
            if id.broker != self.instance || validated.iter().any(|(seen, _, _)| seen == id) {
                return Err(CaptureBrokerError::InvalidAuthorization);
            }
            let authorization = self
                .authorizations
                .get(id)
                .ok_or(CaptureBrokerError::InvalidAuthorization)?;
            if authorization.expires_at < now || &authorization.binding != binding {
                return Err(CaptureBrokerError::BindingMismatch);
            }
            if !unique_required.contains(&authorization.kind)
                || validated
                    .iter()
                    .any(|(_, _, kind)| *kind == authorization.kind)
            {
                return Err(CaptureBrokerError::IncompleteAuthorizationSet);
            }
            self.require_source(&authorization.source_id, authorization.kind)?;
            validated.push((*id, authorization.source_id.clone(), authorization.kind));
        }
        if validated.len() != unique_required.len() {
            return Err(CaptureBrokerError::IncompleteAuthorizationSet);
        }

        let mut grants = Vec::with_capacity(validated.len());
        for (authorization_id, source_id, kind) in validated {
            self.authorizations.remove(&authorization_id);
            let lease_id = CaptureLeaseId {
                broker: self.instance,
                nonce: self.new_nonce(),
            };
            self.leases.insert(
                lease_id,
                Lease {
                    stop: None,
                    binding: binding.clone(),
                    source_id: source_id.clone(),
                    kind,
                    expires_at: now + CAPTURE_LEASE_TTL,
                },
            );
            self.queue_event(CapturePrivacyEvent::Started {
                lease_id,
                app_id: binding.app_id.clone(),
                workspace_id: binding.workspace_id.clone(),
                session_id: binding.session_id.clone(),
                source_id,
                kind,
            });
            grants.push(CaptureLeaseGrant { kind, lease_id });
        }
        Ok(grants)
    }

    /// Resolve a live lease to its local-only backend handle.
    ///
    /// # Errors
    ///
    /// Returns an error for an expired, foreign, mismatched, revoked, or lost lease/source.
    pub fn resolve<'a>(
        &'a self,
        lease_id: CaptureLeaseId,
        binding: &CaptureSessionBinding,
        now: Instant,
    ) -> Result<BorrowedCaptureHandle<'a>, CaptureBrokerError> {
        let lease = self
            .leases
            .get(&lease_id)
            .ok_or(CaptureBrokerError::InvalidLease)?;
        if lease_id.broker != self.instance || &lease.binding != binding || lease.expires_at < now {
            return Err(CaptureBrokerError::InvalidLease);
        }
        if self.is_disabled(lease.kind) {
            return Err(CaptureBrokerError::Disabled);
        }
        let source = self
            .sources
            .get(&lease.source_id)
            .filter(|source| source.view.kind == lease.kind)
            .ok_or(CaptureBrokerError::SourceUnavailable)?;
        Ok(match &source.handle {
            LocalCaptureHandle::PipeWireNode(node) => BorrowedCaptureHandle::PipeWireNode(*node),
            LocalCaptureHandle::FileDescriptor(fd) => {
                BorrowedCaptureHandle::FileDescriptor(fd.as_fd())
            }
            LocalCaptureHandle::SystemDefault(kind) => BorrowedCaptureHandle::SystemDefault(*kind),
        })
    }

    pub fn release(&mut self, lease_id: CaptureLeaseId) -> bool {
        self.revoke_one(lease_id, CaptureRevocation::Released)
    }

    /// Install a local synchronous device stop hook for this exact live lease.
    /// The hook must never call back into this broker; it runs while revocation
    /// owns the lease registry and fences the backend before returning.
    pub(crate) fn stop_on_revoke(
        &mut self,
        lease_id: CaptureLeaseId,
        binding: &CaptureSessionBinding,
        now: Instant,
        stop: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), CaptureBrokerError> {
        self.resolve(lease_id, binding, now)?;
        let lease = self
            .leases
            .get_mut(&lease_id)
            .ok_or(CaptureBrokerError::InvalidLease)?;
        lease.stop = Some(stop);
        Ok(())
    }

    /// Revoke an exact set of unconsumed authorizations minted by one failed
    /// higher-level transaction. Foreign/replayed identifiers are ignored so
    /// rollback cannot disturb another broker instance.
    pub fn revoke_authorizations(&mut self, ids: &[CaptureAuthorizationId]) -> usize {
        ids.iter()
            .filter(|id| id.broker == self.instance)
            .filter(|id| self.authorizations.remove(id).is_some())
            .count()
    }

    /// Revoke only the authorizations and leases for one exact verified
    /// realtime session, leaving other calls by the same app untouched.
    pub fn session_ended(&mut self, binding: &CaptureSessionBinding) {
        self.authorizations
            .retain(|_, authorization| &authorization.binding != binding);
        self.revoke_matching(CaptureRevocation::SessionEnded, |lease| {
            &lease.binding == binding
        });
    }

    pub fn app_exited(&mut self, app_id: &str, workspace_id: &str) {
        self.authorizations.retain(|_, authorization| {
            authorization.binding.app_id != app_id
                || authorization.binding.workspace_id != workspace_id
        });
        self.revoke_matching(CaptureRevocation::AppExited, |lease| {
            lease.binding.app_id == app_id && lease.binding.workspace_id == workspace_id
        });
    }

    pub fn screen_locked(&mut self) {
        self.authorizations.clear();
        self.revoke_matching(CaptureRevocation::ScreenLocked, |_| true);
    }

    pub fn remove_source(&mut self, source_id: &str) -> bool {
        self.authorizations
            .retain(|_, authorization| authorization.source_id != source_id);
        self.revoke_matching(CaptureRevocation::DeviceLost, |lease| {
            lease.source_id == source_id
        });
        self.sources.remove(source_id).is_some()
    }

    pub fn set_kill_switch(&mut self, kind: CaptureKind, disabled: bool) {
        self.disabled.insert(kind, disabled);
        if disabled {
            self.authorizations
                .retain(|_, authorization| authorization.kind != kind);
            self.revoke_matching(CaptureRevocation::KillSwitch, |lease| lease.kind == kind);
        }
    }

    pub fn reap_expired(&mut self, now: Instant) {
        self.authorizations
            .retain(|_, authorization| authorization.expires_at >= now);
        self.revoke_matching(CaptureRevocation::Expired, |lease| lease.expires_at < now);
    }

    pub fn take_privacy_events(&mut self) -> Vec<CapturePrivacyEvent> {
        self.events.drain(..).collect()
    }

    fn require_source(&self, source_id: &str, kind: CaptureKind) -> Result<(), CaptureBrokerError> {
        if self.is_disabled(kind) {
            return Err(CaptureBrokerError::Disabled);
        }
        self.sources
            .get(source_id)
            .filter(|source| source.view.kind == kind)
            .map(|_| ())
            .ok_or(CaptureBrokerError::SourceUnavailable)
    }

    fn is_disabled(&self, kind: CaptureKind) -> bool {
        self.disabled.get(&kind).copied().unwrap_or(false)
    }

    fn revoke_one(&mut self, id: CaptureLeaseId, reason: CaptureRevocation) -> bool {
        let Some(lease) = self.leases.remove(&id) else {
            return false;
        };
        if let Some(stop) = lease.stop.as_ref() {
            stop();
        }
        self.queue_event(stopped_event(id, lease, reason));
        true
    }

    fn revoke_matching(&mut self, reason: CaptureRevocation, predicate: impl Fn(&Lease) -> bool) {
        let ids = self
            .leases
            .iter()
            .filter_map(|(id, lease)| predicate(lease).then_some(*id))
            .collect::<Vec<_>>();
        for id in ids {
            self.revoke_one(id, reason);
        }
    }

    fn new_nonce(&self) -> [u8; 16] {
        loop {
            let nonce = crate::random::nonzero::<16>();
            if !self.authorizations.keys().any(|id| id.nonce == nonce)
                && !self.leases.keys().any(|id| id.nonce == nonce)
            {
                return nonce;
            }
        }
    }

    fn queue_event(&mut self, event: CapturePrivacyEvent) {
        if self.events.len() >= MAX_PRIVACY_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}

fn stopped_event(
    id: CaptureLeaseId,
    lease: Lease,
    reason: CaptureRevocation,
) -> CapturePrivacyEvent {
    CapturePrivacyEvent::Stopped {
        lease_id: id,
        app_id: lease.binding.app_id,
        workspace_id: lease.binding.workspace_id,
        session_id: lease.binding.session_id,
        source_id: lease.source_id,
        kind: lease.kind,
        reason,
    }
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CaptureBrokerError {
    #[error("target-local capture broker is unavailable")]
    Unavailable,
    #[error("invalid verified capture identity")]
    InvalidIdentity,
    #[error("invalid capture source")]
    InvalidSource,
    #[error("capture source is unavailable or has the wrong type")]
    SourceUnavailable,
    #[error("capture is disabled by the local privacy switch")]
    Disabled,
    #[error("fresh local capture authorization is invalid, expired, or already used")]
    InvalidAuthorization,
    #[error("capture authorization binding does not match")]
    BindingMismatch,
    #[error("capture authorization set does not exactly cover the required media")]
    IncompleteAuthorizationSet,
    #[error("capture lease is invalid, expired, or foreign")]
    InvalidLease,
    #[error("capture broker capacity is exhausted")]
    Capacity,
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd as _, AsRawFd as _};

    use super::*;

    fn binding(app: &str, workspace: &str, session: &str) -> CaptureSessionBinding {
        let app = VerifiedAppRegistration::new_verified(app, workspace).unwrap();
        CaptureSessionBinding::from_verified(&app, session, "target-local").unwrap()
    }

    fn broker() -> CaptureBroker {
        let mut broker = CaptureBroker::new();
        broker
            .register_source(
                "camera-front",
                "Front camera",
                CaptureKind::Camera,
                LocalCaptureHandle::PipeWireNode(41),
            )
            .unwrap();
        broker
            .register_source(
                "mic-main",
                "Built-in mic",
                CaptureKind::Microphone,
                LocalCaptureHandle::PipeWireNode(42),
            )
            .unwrap();
        broker
            .register_source(
                "share-audio",
                "System audio",
                CaptureKind::SystemAudio,
                LocalCaptureHandle::PipeWireNode(43),
            )
            .unwrap();
        broker
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn capture_authorization_is_one_shot_exact_and_broker_scoped() {
        // Phase `batch_authorization_is_all_or_nothing`.
        {
            let mut broker = broker();
            let owner = binding("so.tilo.chat", "work", "session-batch");
            let now = Instant::now();
            assert!(
                broker
                    .authorize_batch(
                        &owner,
                        &[
                            ("mic-main".into(), CaptureKind::Microphone),
                            ("missing-camera".into(), CaptureKind::Camera),
                        ],
                        now,
                    )
                    .is_err()
            );
            assert!(broker.authorizations.is_empty());

            let ids = broker
                .authorize_batch(
                    &owner,
                    &[
                        ("mic-main".into(), CaptureKind::Microphone),
                        ("camera-front".into(), CaptureKind::Camera),
                    ],
                    now,
                )
                .unwrap();
            assert_eq!(ids.len(), 2);
            assert_eq!(broker.authorizations.len(), 2);
            assert_eq!(broker.revoke_authorizations(&ids), 2);
            assert!(broker.authorizations.is_empty());
            assert_eq!(broker.revoke_authorizations(&ids), 0);
        }
        // Phase `fresh_authorization_is_one_shot_and_exactly_bound`.
        {
            let mut broker = broker();
            let now = Instant::now();
            let owner = binding("meet", "work", "call-1");
            let other = binding("meet", "work", "call-2");
            let auth = broker
                .authorize(&owner, "camera-front", CaptureKind::Camera, now)
                .unwrap();
            assert_eq!(
                broker.acquire(auth, &other, "camera-front", CaptureKind::Camera, now),
                Err(CaptureBrokerError::BindingMismatch)
            );
            let lease = broker
                .acquire(auth, &owner, "camera-front", CaptureKind::Camera, now)
                .unwrap();
            assert!(matches!(
                broker.resolve(lease, &owner, now),
                Ok(BorrowedCaptureHandle::PipeWireNode(41))
            ));
        }
        // Phase `authorization_expires_and_cannot_cross_brokers`.
        {
            let now = Instant::now();
            let owner = binding("meet", "work", "call");
            let mut first = broker();
            let auth = first
                .authorize(&owner, "mic-main", CaptureKind::Microphone, now)
                .unwrap();
            assert_eq!(
                first.acquire(
                    auth,
                    &owner,
                    "mic-main",
                    CaptureKind::Microphone,
                    now + CAPTURE_AUTHORIZATION_TTL + Duration::from_millis(1)
                ),
                Err(CaptureBrokerError::InvalidAuthorization)
            );
            let mut second = broker();
            assert_eq!(
                second.acquire(auth, &owner, "mic-main", CaptureKind::Microphone, now),
                Err(CaptureBrokerError::InvalidAuthorization)
            );
        }
        // Phase `opaque_authorizations_are_canonical_restart_scoped_capabilities`.
        {
            let now = Instant::now();
            let owner = binding("meet", "work", "call");
            let mut broker = broker();
            let authorization = broker
                .authorize(&owner, "mic-main", CaptureKind::Microphone, now)
                .unwrap();
            let opaque = authorization.to_opaque();
            assert_eq!(opaque.len(), OPAQUE_ID_BYTES * 2);
            assert_eq!(
                CaptureAuthorizationId::from_opaque(&opaque).unwrap(),
                authorization
            );
            assert_eq!(
                CaptureAuthorizationId::from_opaque(&opaque.to_ascii_uppercase()),
                Err(CaptureBrokerError::InvalidAuthorization)
            );
            assert_eq!(
                CaptureAuthorizationId::from_opaque("00"),
                Err(CaptureBrokerError::InvalidAuthorization)
            );
        }
        // Phase `required_set_is_atomic_exact_bound_and_replay_safe`.
        {
            let now = Instant::now();
            let owner = binding("meet", "work", "call-1");
            let wrong_session = binding("meet", "work", "call-2");
            let wrong_target = CaptureSessionBinding::from_verified(
                &VerifiedAppRegistration::new_verified("meet", "work").unwrap(),
                "call-1",
                "target-other",
            )
            .unwrap();
            let mut broker = broker();
            let microphone = broker
                .authorize(&owner, "mic-main", CaptureKind::Microphone, now)
                .unwrap();
            let camera = broker
                .authorize(&owner, "camera-front", CaptureKind::Camera, now)
                .unwrap();
            let required = [CaptureKind::Microphone, CaptureKind::Camera];

            assert_eq!(
                broker.acquire_required(&[microphone], &owner, &required, now),
                Err(CaptureBrokerError::IncompleteAuthorizationSet)
            );
            assert_eq!(
                broker.acquire_required(&[microphone, camera], &wrong_session, &required, now),
                Err(CaptureBrokerError::BindingMismatch)
            );
            assert_eq!(
                broker.acquire_required(&[microphone, camera], &wrong_target, &required, now),
                Err(CaptureBrokerError::BindingMismatch)
            );
            let grants = broker
                .acquire_required(&[microphone, camera], &owner, &required, now)
                .unwrap();
            assert_eq!(grants.len(), 2);
            assert!(
                grants
                    .iter()
                    .all(|grant| broker.resolve(grant.lease_id, &owner, now).is_ok())
            );
            assert_eq!(
                broker.acquire_required(&[microphone, camera], &owner, &required, now),
                Err(CaptureBrokerError::InvalidAuthorization)
            );
        }
    }

    #[test]
    fn capture_sources_stay_local_kind_scoped_and_lease_bound() {
        // Phase `source_kinds_are_distinct_and_handles_stay_local`.
        {
            let broker = broker();
            assert_eq!(broker.enumerate(CaptureKind::Camera)[0].id, "camera-front");
            assert_eq!(broker.enumerate(CaptureKind::Microphone)[0].id, "mic-main");
            assert_eq!(
                broker.enumerate(CaptureKind::SystemAudio)[0].id,
                "share-audio"
            );
            assert!(broker.enumerate(CaptureKind::Screen).is_empty());
        }
        // Phase `kill_switch_is_kind_local_and_blocks_enumeration_and_authorization`.
        {
            let mut broker = broker();
            let owner = binding("meet", "work", "call");
            broker.set_kill_switch(CaptureKind::Camera, true);
            assert!(broker.enumerate(CaptureKind::Camera).is_empty());
            assert_eq!(
                broker.authorize(&owner, "camera-front", CaptureKind::Camera, Instant::now()),
                Err(CaptureBrokerError::Disabled)
            );
            assert_eq!(broker.enumerate(CaptureKind::Microphone).len(), 1);
        }
        // Phase `owned_fd_is_borrowed_only_while_lease_and_broker_live`.
        {
            let file = std::fs::File::open("/dev/null").unwrap();
            let expected = file.as_fd().as_raw_fd();
            let mut broker = CaptureBroker::new();
            broker
                .register_source(
                    "screen",
                    "Screen",
                    CaptureKind::Screen,
                    LocalCaptureHandle::FileDescriptor(file.into()),
                )
                .unwrap();
            let owner = binding("share", "work", "room");
            let now = Instant::now();
            let auth = broker
                .authorize(&owner, "screen", CaptureKind::Screen, now)
                .unwrap();
            let lease = broker
                .acquire(auth, &owner, "screen", CaptureKind::Screen, now)
                .unwrap();
            let BorrowedCaptureHandle::FileDescriptor(fd) =
                broker.resolve(lease, &owner, now).unwrap()
            else {
                panic!("expected fd");
            };
            assert_eq!(fd.as_raw_fd(), expected);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn capture_teardown_revokes_exact_leases_and_is_audited() {
        // Phase `exact_session_teardown_revokes_only_that_calls_leases`.
        {
            let now = Instant::now();
            let first = binding("meet", "work", "call-1");
            let second = binding("meet", "work", "call-2");
            let mut broker = broker();
            let first_authorization = broker
                .authorize(&first, "mic-main", CaptureKind::Microphone, now)
                .unwrap();
            let second_authorization = broker
                .authorize(&second, "camera-front", CaptureKind::Camera, now)
                .unwrap();
            let first_lease = broker
                .acquire(
                    first_authorization,
                    &first,
                    "mic-main",
                    CaptureKind::Microphone,
                    now,
                )
                .unwrap();
            let second_lease = broker
                .acquire(
                    second_authorization,
                    &second,
                    "camera-front",
                    CaptureKind::Camera,
                    now,
                )
                .unwrap();

            broker.session_ended(&first);
            assert!(matches!(
                broker.resolve(first_lease, &first, now),
                Err(CaptureBrokerError::InvalidLease)
            ));
            assert!(broker.resolve(second_lease, &second, now).is_ok());
            assert!(broker.take_privacy_events().iter().any(|event| matches!(
                event,
                CapturePrivacyEvent::Stopped {
                    lease_id,
                    reason: CaptureRevocation::SessionEnded,
                    ..
                } if *lease_id == first_lease
            )));
        }
        // Phase `every_teardown_path_revokes_and_emits_an_auditable_stop`.
        {
            let now = Instant::now();
            let owner = binding("meet", "work", "call");
            for reason in [
                CaptureRevocation::SessionEnded,
                CaptureRevocation::AppExited,
                CaptureRevocation::ScreenLocked,
                CaptureRevocation::DeviceLost,
                CaptureRevocation::KillSwitch,
                CaptureRevocation::Expired,
            ] {
                let mut broker = broker();
                let auth = broker
                    .authorize(&owner, "mic-main", CaptureKind::Microphone, now)
                    .unwrap();
                let lease = broker
                    .acquire(auth, &owner, "mic-main", CaptureKind::Microphone, now)
                    .unwrap();
                match reason {
                    CaptureRevocation::SessionEnded => broker.session_ended(&owner),
                    CaptureRevocation::AppExited => broker.app_exited("meet", "work"),
                    CaptureRevocation::ScreenLocked => broker.screen_locked(),
                    CaptureRevocation::DeviceLost => {
                        broker.remove_source("mic-main");
                    }
                    CaptureRevocation::KillSwitch => {
                        broker.set_kill_switch(CaptureKind::Microphone, true)
                    }
                    CaptureRevocation::Expired => {
                        broker.reap_expired(now + CAPTURE_LEASE_TTL + Duration::from_millis(1))
                    }
                    CaptureRevocation::Released => unreachable!(),
                }
                assert!(matches!(
                    broker.resolve(lease, &owner, now),
                    Err(CaptureBrokerError::InvalidLease)
                ));
                let events = broker.take_privacy_events();
                assert!(
                    matches!(events.as_slice(), [CapturePrivacyEvent::Started { .. }, CapturePrivacyEvent::Stopped { reason: event_reason, .. }] if *event_reason == reason)
                );
            }
        }
        // Phase `replacement_invalidates_old_proof_and_audit_queue_is_bounded`.
        {
            let now = Instant::now();
            let owner = binding("meet", "work", "call");
            let mut broker = broker();
            let old = broker
                .authorize(&owner, "mic-main", CaptureKind::Microphone, now)
                .unwrap();
            broker
                .register_source(
                    "mic-main",
                    "Replacement mic",
                    CaptureKind::Microphone,
                    LocalCaptureHandle::PipeWireNode(99),
                )
                .unwrap();
            assert_eq!(
                broker.acquire(old, &owner, "mic-main", CaptureKind::Microphone, now),
                Err(CaptureBrokerError::InvalidAuthorization)
            );
            for _ in 0..MAX_PRIVACY_EVENTS.saturating_add(8) {
                let auth = broker
                    .authorize(&owner, "mic-main", CaptureKind::Microphone, now)
                    .unwrap();
                let lease = broker
                    .acquire(auth, &owner, "mic-main", CaptureKind::Microphone, now)
                    .unwrap();
                assert!(broker.release(lease));
            }
            assert_eq!(broker.events.len(), MAX_PRIVACY_EVENTS);
        }
    }
}
