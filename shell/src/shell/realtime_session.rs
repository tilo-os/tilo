//! Framework-owned continuity for active calls, meetings, and live rooms.
//!
//! This module is deliberately transport agnostic. It coordinates semantic
//! room resumption and cutover, but never represents a `PipeWire` node, portal
//! object, file descriptor, or capture stream. Those are target-local objects
//! that must be freshly authorized after an accept.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use rand_core::{OsRng, RngCore};
use thiserror::Error;

use crate::shell::personal_continuity::DeviceId;

pub const MAX_REALTIME_SESSIONS: usize = 64;
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(2 * 60 * 60);
pub const DEFAULT_CUTOVER_TIMEOUT: Duration = Duration::from_secs(15);
pub const CAPTURE_AUTHORIZATION_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BrokerInstanceId([u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RealtimeSessionId {
    broker: BrokerInstanceId,
    nonce: [u8; 16],
}

impl RealtimeSessionId {
    #[must_use]
    pub const fn broker(self) -> BrokerInstanceId {
        self.broker
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaptureAuthorizationId {
    broker: BrokerInstanceId,
    nonce: [u8; 16],
}

/// Proof that package verification and workspace resolution happened at the
/// shell boundary. Fields are private so an app cannot mint another app's
/// identity. The launcher/broker integration is the intended constructor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAppRegistration {
    app_id: String,
    workspace_id: String,
}

impl VerifiedAppRegistration {
    /// Construct only after the caller verified the installed package and
    /// resolved the workspace through trusted shell state.
    #[allow(
        dead_code,
        reason = "the constructor stays crate-private until launcher integration wires verified manifests into this broker"
    )]
    pub(crate) fn new_verified(
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
    ) -> Result<Self, RealtimeSessionError> {
        let registration = Self {
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
        };
        if !valid_identifier(&registration.app_id, 256)
            || !valid_identifier(&registration.workspace_id, 128)
        {
            return Err(RealtimeSessionError::InvalidRegistration);
        }
        Ok(registration)
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}

/// A stable provider/room pair, not a join URL or bearer credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticRoomLocator {
    provider: String,
    room_id: String,
}

impl SemanticRoomLocator {
    /// # Errors
    ///
    /// Returns an error when either semantic identifier is empty, oversized,
    /// contains control characters, or resembles a URL/bearer locator.
    pub fn new(
        provider: impl Into<String>,
        room_id: impl Into<String>,
    ) -> Result<Self, RealtimeSessionError> {
        let locator = Self {
            provider: provider.into(),
            room_id: room_id.into(),
        };
        if !valid_identifier(&locator.provider, 128)
            || !valid_identifier(&locator.room_id, 512)
            || locator.room_id.contains("://")
            || locator.room_id.contains('?')
        {
            return Err(RealtimeSessionError::InvalidRoomLocator);
        }
        Ok(locator)
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub fn room_id(&self) -> &str {
        &self.room_id
    }
}

/// Independent capture/playback facets for a realtime room. A normal call may
/// use every facet simultaneously; share/system audio never implies mic use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealtimeMediaSet {
    bits: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimeMediaFacet {
    Microphone,
    Camera,
    ScreenShare,
    ShareAudio,
}

impl RealtimeMediaSet {
    /// # Errors
    ///
    /// Returns an error when no media is active or share audio is requested
    /// without a screen share to which it can belong.
    pub fn new(
        facets: impl IntoIterator<Item = RealtimeMediaFacet>,
    ) -> Result<Self, RealtimeSessionError> {
        let bits = facets.into_iter().fold(0_u8, |bits, facet| {
            bits | match facet {
                RealtimeMediaFacet::Microphone => 1 << 0,
                RealtimeMediaFacet::Camera => 1 << 1,
                RealtimeMediaFacet::ScreenShare => 1 << 2,
                RealtimeMediaFacet::ShareAudio => 1 << 3,
            }
        });
        let media = Self { bits };
        if !(media.microphone() || media.camera() || media.screen_share())
            || (media.share_audio() && !media.screen_share())
        {
            return Err(RealtimeSessionError::InvalidMediaSet);
        }
        Ok(media)
    }

    #[must_use]
    pub const fn microphone(self) -> bool {
        self.bits & (1 << 0) != 0
    }

    #[must_use]
    pub const fn camera(self) -> bool {
        self.bits & (1 << 1) != 0
    }

    #[must_use]
    pub const fn screen_share(self) -> bool {
        self.bits & (1 << 2) != 0
    }

    #[must_use]
    pub const fn share_audio(self) -> bool {
        self.bits & (1 << 3) != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeResumeIntent {
    pub room: SemanticRoomLocator,
    /// App-owned semantic state such as "stage" or "breakout-2". It must not
    /// contain a URL, access token, or process-local media identifier.
    pub semantic_position: Option<String>,
    pub media: RealtimeMediaSet,
}

impl RealtimeResumeIntent {
    /// # Errors
    ///
    /// Returns an error for unsafe or oversized semantic state.
    pub fn validate(&self) -> Result<(), RealtimeSessionError> {
        if self.semantic_position.as_ref().is_some_and(|position| {
            position.is_empty()
                || position.len() > 512
                || position.chars().any(char::is_control)
                || position.contains("://")
                || position.contains('?')
        }) {
            return Err(RealtimeSessionError::InvalidResumeIntent);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimeMediaAction {
    SetMicrophoneMuted(bool),
    SetCameraMuted(bool),
    SetScreenSharePaused(bool),
    SetShareAudioMuted(bool),
    Leave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimeSessionPhase {
    SourceLive,
    AwaitingTargetAccept,
    AwaitingTargetMedia,
    TargetLive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeSessionView {
    pub id: RealtimeSessionId,
    pub app_id: String,
    pub workspace_id: String,
    pub source: DeviceId,
    pub target: Option<DeviceId>,
    pub intent: RealtimeResumeIntent,
    pub phase: RealtimeSessionPhase,
    pub source_live: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutoverDirective {
    pub session_id: RealtimeSessionId,
    pub app_id: String,
    pub workspace_id: String,
    pub source: DeviceId,
    pub target: DeviceId,
    /// This becomes true only after exact-target `MediaReady` succeeds.
    pub stop_source: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrokerEventKind {
    RolledBackToSource {
        target: DeviceId,
    },
    Ended {
        live_device: DeviceId,
    },
    /// Registry TTL elapsed; the broker forgets coordination state without
    /// claiming that an independently live media endpoint stopped.
    ForgottenLive {
        live_device: DeviceId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerEvent {
    pub session_id: RealtimeSessionId,
    pub kind: BrokerEventKind,
}

#[derive(Clone, Debug)]
enum SessionState {
    SourceLive,
    AwaitingAccept {
        target: DeviceId,
        deadline: Instant,
    },
    AwaitingMedia {
        target: DeviceId,
        authorization: CaptureAuthorizationId,
        deadline: Instant,
    },
    TargetLive {
        target: DeviceId,
    },
}

impl SessionState {
    fn phase(&self) -> RealtimeSessionPhase {
        match self {
            Self::SourceLive => RealtimeSessionPhase::SourceLive,
            Self::AwaitingAccept { .. } => RealtimeSessionPhase::AwaitingTargetAccept,
            Self::AwaitingMedia { .. } => RealtimeSessionPhase::AwaitingTargetMedia,
            Self::TargetLive { .. } => RealtimeSessionPhase::TargetLive,
        }
    }

    fn target(&self) -> Option<&DeviceId> {
        match self {
            Self::SourceLive => None,
            Self::AwaitingAccept { target, .. }
            | Self::AwaitingMedia { target, .. }
            | Self::TargetLive { target } => Some(target),
        }
    }
}

#[derive(Clone, Debug)]
struct SessionEntry {
    app: VerifiedAppRegistration,
    source: DeviceId,
    intent: RealtimeResumeIntent,
    state: SessionState,
    last_activity: Instant,
    last_action: Option<RealtimeMediaAction>,
}

#[derive(Clone, Debug)]
struct CaptureAuthorization {
    session_id: RealtimeSessionId,
    app: VerifiedAppRegistration,
    source: DeviceId,
    target: DeviceId,
    expires: Instant,
}

pub struct RealtimeSessionBroker {
    instance: BrokerInstanceId,
    sessions: BTreeMap<RealtimeSessionId, SessionEntry>,
    capture_authorizations: BTreeMap<CaptureAuthorizationId, CaptureAuthorization>,
    max_sessions: usize,
    session_ttl: Duration,
    cutover_timeout: Duration,
}

impl Default for RealtimeSessionBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl RealtimeSessionBroker {
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(
            MAX_REALTIME_SESSIONS,
            DEFAULT_SESSION_TTL,
            DEFAULT_CUTOVER_TIMEOUT,
        )
    }

    #[must_use]
    pub fn with_limits(
        max_sessions: usize,
        session_ttl: Duration,
        cutover_timeout: Duration,
    ) -> Self {
        Self {
            instance: BrokerInstanceId(random_nonzero()),
            sessions: BTreeMap::new(),
            capture_authorizations: BTreeMap::new(),
            max_sessions: max_sessions.clamp(1, MAX_REALTIME_SESSIONS),
            session_ttl: session_ttl.max(Duration::from_millis(1)),
            cutover_timeout: cutover_timeout.max(Duration::from_millis(1)),
        }
    }

    #[must_use]
    pub const fn instance_id(&self) -> BrokerInstanceId {
        self.instance
    }

    /// Register one currently live source session.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid semantic state or capacity exhaustion.
    pub fn register(
        &mut self,
        app: &VerifiedAppRegistration,
        source: DeviceId,
        intent: RealtimeResumeIntent,
        now: Instant,
    ) -> Result<RealtimeSessionId, RealtimeSessionError> {
        intent.validate()?;
        if self.sessions.len() >= self.max_sessions {
            return Err(RealtimeSessionError::RegistryFull);
        }
        let id = self.new_session_id();
        self.sessions.insert(
            id,
            SessionEntry {
                app: app.clone(),
                source,
                intent,
                state: SessionState::SourceLive,
                last_activity: now,
                last_action: None,
            },
        );
        Ok(id)
    }

    /// Begin handoff while deliberately leaving the source live.
    ///
    /// # Errors
    ///
    /// Returns an error unless the exact verified app, workspace, source and
    /// restart-scoped session are currently source-live.
    pub fn propose_target(
        &mut self,
        app: &VerifiedAppRegistration,
        session_id: RealtimeSessionId,
        source: &DeviceId,
        target: DeviceId,
        now: Instant,
    ) -> Result<(), RealtimeSessionError> {
        if source == &target {
            return Err(RealtimeSessionError::InvalidTarget);
        }
        let deadline = deadline(now, self.cutover_timeout);
        let entry = self.bound_entry_mut(app, session_id, source)?;
        if !matches!(entry.state, SessionState::SourceLive) {
            return Err(RealtimeSessionError::InvalidTransition);
        }
        entry.state = SessionState::AwaitingAccept { target, deadline };
        entry.last_activity = now;
        Ok(())
    }

    /// Mint a short-lived, one-shot proof of fresh target-side capture consent.
    /// The proof authorizes no transport object and contains no media handle.
    ///
    /// # Errors
    ///
    /// Returns an error unless the exact target is awaiting acceptance.
    pub fn authorize_target_capture(
        &mut self,
        app: &VerifiedAppRegistration,
        session_id: RealtimeSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<CaptureAuthorizationId, RealtimeSessionError> {
        let entry = self.bound_entry(app, session_id, source)?;
        if !matches!(&entry.state, SessionState::AwaitingAccept { target: expected, deadline } if expected == target && now <= *deadline)
        {
            return Err(RealtimeSessionError::InvalidTransition);
        }
        self.capture_authorizations
            .retain(|_, authorization| authorization.session_id != session_id);
        let id = self.new_capture_authorization_id();
        self.capture_authorizations.insert(
            id,
            CaptureAuthorization {
                session_id,
                app: app.clone(),
                source: source.clone(),
                target: target.clone(),
                expires: deadline(now, CAPTURE_AUTHORIZATION_TTL),
            },
        );
        Ok(id)
    }

    /// Phase one: the exact target accepts using fresh local capture consent.
    /// Source media remains live after this call.
    ///
    /// # Errors
    ///
    /// Returns an error for stale/replayed consent or any binding mismatch.
    pub fn accept_target(
        &mut self,
        app: &VerifiedAppRegistration,
        session_id: RealtimeSessionId,
        source: &DeviceId,
        target: &DeviceId,
        authorization_id: CaptureAuthorizationId,
        now: Instant,
    ) -> Result<(), RealtimeSessionError> {
        let entry = self.bound_entry(app, session_id, source)?;
        if !matches!(&entry.state, SessionState::AwaitingAccept { target: expected, deadline } if expected == target && now <= *deadline)
        {
            return Err(RealtimeSessionError::InvalidTransition);
        }
        let authorization = self
            .capture_authorizations
            .get(&authorization_id)
            .cloned()
            .ok_or(RealtimeSessionError::CaptureAuthorizationRequired)?;
        if authorization_id.broker != self.instance
            || authorization.session_id != session_id
            || &authorization.app != app
            || &authorization.source != source
            || &authorization.target != target
            || now > authorization.expires
        {
            return Err(RealtimeSessionError::CaptureAuthorizationRequired);
        }
        self.capture_authorizations.remove(&authorization_id);
        let cutover_timeout = self.cutover_timeout;
        let entry = self.bound_entry_mut(app, session_id, source)?;
        entry.state = SessionState::AwaitingMedia {
            target: target.clone(),
            authorization: authorization_id,
            deadline: deadline(now, cutover_timeout),
        };
        entry.last_activity = now;
        Ok(())
    }

    /// Phase two: only proven target media readiness permits source cutover.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale deadline, consumed/wrong authorization, or
    /// any source/target/app/session mismatch. Failure never stops the source.
    pub fn media_ready(
        &mut self,
        app: &VerifiedAppRegistration,
        session_id: RealtimeSessionId,
        source: &DeviceId,
        target: &DeviceId,
        authorization_id: CaptureAuthorizationId,
        now: Instant,
    ) -> Result<CutoverDirective, RealtimeSessionError> {
        let entry = self.bound_entry_mut(app, session_id, source)?;
        if !matches!(&entry.state, SessionState::AwaitingMedia { target: expected, authorization, deadline } if expected == target && *authorization == authorization_id && now <= *deadline)
        {
            return Err(RealtimeSessionError::InvalidTransition);
        }
        entry.state = SessionState::TargetLive {
            target: target.clone(),
        };
        entry.last_activity = now;
        Ok(CutoverDirective {
            session_id,
            app_id: app.app_id.clone(),
            workspace_id: app.workspace_id.clone(),
            source: source.clone(),
            target: target.clone(),
            stop_source: true,
        })
    }

    /// Roll back a target setup failure immediately. The source remains live.
    ///
    /// # Errors
    ///
    /// Returns an error unless the exact target is in either pre-cutover
    /// phase for the exact app/workspace/source/session binding.
    pub fn target_failed(
        &mut self,
        app: &VerifiedAppRegistration,
        session_id: RealtimeSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<BrokerEvent, RealtimeSessionError> {
        let entry = self.bound_entry_mut(app, session_id, source)?;
        if !matches!(&entry.state, SessionState::AwaitingAccept { target: expected, .. } | SessionState::AwaitingMedia { target: expected, .. } if expected == target)
        {
            return Err(RealtimeSessionError::InvalidTransition);
        }
        entry.state = SessionState::SourceLive;
        entry.last_activity = now;
        self.revoke_session_authorizations(session_id);
        Ok(BrokerEvent {
            session_id,
            kind: BrokerEventKind::RolledBackToSource {
                target: target.clone(),
            },
        })
    }

    /// Apply a mode-compatible action from the one currently live endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error for a binding mismatch, non-live caller, or action not
    /// supported by the registered media mode.
    pub fn apply_media_action(
        &mut self,
        app: &VerifiedAppRegistration,
        session_id: RealtimeSessionId,
        device: &DeviceId,
        action: RealtimeMediaAction,
        now: Instant,
    ) -> Result<Option<BrokerEvent>, RealtimeSessionError> {
        let entry = self
            .sessions
            .get_mut(&session_id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        if &entry.app != app || live_device(entry) != device {
            return Err(RealtimeSessionError::BindingMismatch);
        }
        let supported = match action {
            RealtimeMediaAction::SetMicrophoneMuted(_) => entry.intent.media.microphone(),
            RealtimeMediaAction::SetCameraMuted(_) => entry.intent.media.camera(),
            RealtimeMediaAction::SetScreenSharePaused(_) => entry.intent.media.screen_share(),
            RealtimeMediaAction::SetShareAudioMuted(_) => entry.intent.media.share_audio(),
            RealtimeMediaAction::Leave => true,
        };
        if !supported {
            return Err(RealtimeSessionError::UnsupportedMediaAction);
        }
        if action == RealtimeMediaAction::Leave {
            self.sessions.remove(&session_id);
            self.revoke_session_authorizations(session_id);
            return Ok(Some(BrokerEvent {
                session_id,
                kind: BrokerEventKind::Ended {
                    live_device: device.clone(),
                },
            }));
        }
        entry.last_action = Some(action);
        entry.last_activity = now;
        Ok(None)
    }

    /// Process cutover deadlines and inactive registry entries.
    #[must_use]
    pub fn tick(&mut self, now: Instant) -> Vec<BrokerEvent> {
        self.capture_authorizations
            .retain(|_, authorization| now <= authorization.expires);
        let mut events = Vec::new();
        let ids = self.sessions.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let Some(entry) = self.sessions.get_mut(&id) else {
                continue;
            };
            let timed_out_target = match &entry.state {
                SessionState::AwaitingAccept { target, deadline }
                | SessionState::AwaitingMedia {
                    target, deadline, ..
                } if now > *deadline => Some(target.clone()),
                _ => None,
            };
            if let Some(target) = timed_out_target {
                entry.state = SessionState::SourceLive;
                entry.last_activity = now;
                self.revoke_session_authorizations(id);
                events.push(BrokerEvent {
                    session_id: id,
                    kind: BrokerEventKind::RolledBackToSource { target },
                });
                continue;
            }
            if now.saturating_duration_since(entry.last_activity) > self.session_ttl {
                let live_device = live_device(entry).clone();
                self.sessions.remove(&id);
                self.revoke_session_authorizations(id);
                events.push(BrokerEvent {
                    session_id: id,
                    kind: BrokerEventKind::ForgottenLive { live_device },
                });
            }
        }
        events
    }

    /// Privacy hook for a local lock. Pending target work rolls back when the
    /// target locks; locking the live endpoint tears down the broker entry.
    #[must_use]
    pub fn device_locked(&mut self, device: &DeviceId, now: Instant) -> Vec<BrokerEvent> {
        self.device_unavailable(device, now)
    }

    /// Connectivity hook with the same fail-closed semantics as locking.
    #[must_use]
    pub fn device_lost(&mut self, device: &DeviceId, now: Instant) -> Vec<BrokerEvent> {
        self.device_unavailable(device, now)
    }

    #[must_use]
    pub fn session(&self, id: RealtimeSessionId) -> Option<RealtimeSessionView> {
        let entry = self.sessions.get(&id)?;
        Some(RealtimeSessionView {
            id,
            app_id: entry.app.app_id.clone(),
            workspace_id: entry.app.workspace_id.clone(),
            source: entry.source.clone(),
            target: entry.state.target().cloned(),
            intent: entry.intent.clone(),
            phase: entry.state.phase(),
            source_live: !matches!(entry.state, SessionState::TargetLive { .. }),
        })
    }

    fn device_unavailable(&mut self, device: &DeviceId, now: Instant) -> Vec<BrokerEvent> {
        let mut events = Vec::new();
        let ids = self.sessions.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let Some(entry) = self.sessions.get_mut(&id) else {
                continue;
            };
            let pending_target = match &entry.state {
                SessionState::AwaitingAccept { target, .. }
                | SessionState::AwaitingMedia { target, .. }
                    if target == device =>
                {
                    Some(target.clone())
                }
                _ => None,
            };
            if let Some(target) = pending_target {
                entry.state = SessionState::SourceLive;
                entry.last_activity = now;
                self.revoke_session_authorizations(id);
                events.push(BrokerEvent {
                    session_id: id,
                    kind: BrokerEventKind::RolledBackToSource { target },
                });
                continue;
            }
            if live_device(entry) == device {
                self.sessions.remove(&id);
                self.revoke_session_authorizations(id);
                events.push(BrokerEvent {
                    session_id: id,
                    kind: BrokerEventKind::Ended {
                        live_device: device.clone(),
                    },
                });
            }
        }
        events
    }

    fn bound_entry(
        &self,
        app: &VerifiedAppRegistration,
        id: RealtimeSessionId,
        source: &DeviceId,
    ) -> Result<&SessionEntry, RealtimeSessionError> {
        if id.broker != self.instance {
            return Err(RealtimeSessionError::UnknownSession);
        }
        let entry = self
            .sessions
            .get(&id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        if &entry.app != app || &entry.source != source {
            return Err(RealtimeSessionError::BindingMismatch);
        }
        Ok(entry)
    }

    fn bound_entry_mut(
        &mut self,
        app: &VerifiedAppRegistration,
        id: RealtimeSessionId,
        source: &DeviceId,
    ) -> Result<&mut SessionEntry, RealtimeSessionError> {
        if id.broker != self.instance {
            return Err(RealtimeSessionError::UnknownSession);
        }
        let entry = self
            .sessions
            .get_mut(&id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        if &entry.app != app || &entry.source != source {
            return Err(RealtimeSessionError::BindingMismatch);
        }
        Ok(entry)
    }

    fn revoke_session_authorizations(&mut self, session_id: RealtimeSessionId) {
        self.capture_authorizations
            .retain(|_, authorization| authorization.session_id != session_id);
    }

    fn new_session_id(&self) -> RealtimeSessionId {
        loop {
            let id = RealtimeSessionId {
                broker: self.instance,
                nonce: random_nonzero(),
            };
            if !self.sessions.contains_key(&id) {
                return id;
            }
        }
    }

    fn new_capture_authorization_id(&self) -> CaptureAuthorizationId {
        loop {
            let id = CaptureAuthorizationId {
                broker: self.instance,
                nonce: random_nonzero(),
            };
            if !self.capture_authorizations.contains_key(&id) {
                return id;
            }
        }
    }
}

fn live_device(entry: &SessionEntry) -> &DeviceId {
    match &entry.state {
        SessionState::SourceLive
        | SessionState::AwaitingAccept { .. }
        | SessionState::AwaitingMedia { .. } => &entry.source,
        SessionState::TargetLive { target } => target,
    }
}

fn random_nonzero() -> [u8; 16] {
    let mut value = [0; 16];
    while value == [0; 16] {
        OsRng.fill_bytes(&mut value);
    }
    value
}

fn deadline(now: Instant, duration: Duration) -> Instant {
    now.checked_add(duration).unwrap_or(now)
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.chars().any(char::is_control)
        && !value.trim().is_empty()
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RealtimeSessionError {
    #[error("verified app or workspace registration is invalid")]
    InvalidRegistration,
    #[error("semantic room locator is invalid")]
    InvalidRoomLocator,
    #[error("resume intent is invalid or contains an unsafe locator")]
    InvalidResumeIntent,
    #[error("realtime media set is empty or internally inconsistent")]
    InvalidMediaSet,
    #[error("realtime session registry is full")]
    RegistryFull,
    #[error("realtime session is unknown in this broker restart")]
    UnknownSession,
    #[error("app, workspace, device, or session binding does not match")]
    BindingMismatch,
    #[error("target device is invalid")]
    InvalidTarget,
    #[error("realtime session transition is invalid or expired")]
    InvalidTransition,
    #[error("fresh target capture authorization is required")]
    CaptureAuthorizationRequired,
    #[error("media action is unsupported by this session mode")]
    UnsupportedMediaAction,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCallingApp {
        registration: VerifiedAppRegistration,
    }

    impl FakeCallingApp {
        fn verified(app_id: &str, workspace: &str) -> Self {
            Self {
                registration: VerifiedAppRegistration::new_verified(app_id, workspace).unwrap(),
            }
        }

        fn intent(media: RealtimeMediaSet) -> RealtimeResumeIntent {
            RealtimeResumeIntent {
                room: SemanticRoomLocator::new("meet", "room-42").unwrap(),
                semantic_position: Some("main-stage".into()),
                media,
            }
        }

        fn audio() -> RealtimeMediaSet {
            RealtimeMediaSet::new([RealtimeMediaFacet::Microphone]).unwrap()
        }

        fn audio_video() -> RealtimeMediaSet {
            RealtimeMediaSet::new([RealtimeMediaFacet::Microphone, RealtimeMediaFacet::Camera])
                .unwrap()
        }
    }

    fn device(name: &str) -> DeviceId {
        DeviceId::new(name).unwrap()
    }

    #[test]
    fn cutover_requires_accept_then_media_ready_and_keeps_source_live_until_ready() {
        let now = Instant::now();
        let app = FakeCallingApp::verified("dev.tilo.calls", "work");
        let source = device("laptop");
        let target = device("tablet");
        let mut broker = RealtimeSessionBroker::new();
        let id = broker
            .register(
                &app.registration,
                source.clone(),
                FakeCallingApp::intent(
                    RealtimeMediaSet::new([
                        RealtimeMediaFacet::Microphone,
                        RealtimeMediaFacet::Camera,
                        RealtimeMediaFacet::ScreenShare,
                        RealtimeMediaFacet::ShareAudio,
                    ])
                    .unwrap(),
                ),
                now,
            )
            .unwrap();
        broker
            .propose_target(&app.registration, id, &source, target.clone(), now)
            .unwrap();
        assert!(broker.session(id).unwrap().source_live);
        assert_eq!(
            broker.media_ready(
                &app.registration,
                id,
                &source,
                &target,
                CaptureAuthorizationId {
                    broker: broker.instance_id(),
                    nonce: [9; 16],
                },
                now,
            ),
            Err(RealtimeSessionError::InvalidTransition)
        );
        let authorization = broker
            .authorize_target_capture(&app.registration, id, &source, &target, now)
            .unwrap();
        broker
            .accept_target(&app.registration, id, &source, &target, authorization, now)
            .unwrap();
        assert!(broker.session(id).unwrap().source_live);
        let cutover = broker
            .media_ready(&app.registration, id, &source, &target, authorization, now)
            .unwrap();
        assert!(cutover.stop_source);
        assert!(!broker.session(id).unwrap().source_live);
    }

    #[test]
    fn timeout_and_target_loss_roll_back_without_stopping_source() {
        let now = Instant::now();
        let app = FakeCallingApp::verified("dev.tilo.calls", "work");
        let source = device("laptop");
        let target = device("tablet");
        let mut broker =
            RealtimeSessionBroker::with_limits(4, Duration::from_secs(60), Duration::from_secs(2));
        let id = broker
            .register(
                &app.registration,
                source.clone(),
                FakeCallingApp::intent(FakeCallingApp::audio()),
                now,
            )
            .unwrap();
        broker
            .propose_target(&app.registration, id, &source, target.clone(), now)
            .unwrap();
        let events = broker.tick(now + Duration::from_secs(3));
        assert!(matches!(
            events.as_slice(),
            [BrokerEvent { kind: BrokerEventKind::RolledBackToSource { target: rolled_back }, .. }]
                if rolled_back == &target
        ));
        assert!(broker.session(id).unwrap().source_live);

        broker
            .propose_target(
                &app.registration,
                id,
                &source,
                target.clone(),
                now + Duration::from_secs(4),
            )
            .unwrap();
        let events = broker.device_lost(&target, now + Duration::from_secs(4));
        assert!(matches!(
            events.first().map(|event| &event.kind),
            Some(BrokerEventKind::RolledBackToSource { .. })
        ));
        assert!(broker.session(id).unwrap().source_live);

        let retry_at = now + Duration::from_secs(5);
        broker
            .propose_target(&app.registration, id, &source, target.clone(), retry_at)
            .unwrap();
        let authorization = broker
            .authorize_target_capture(&app.registration, id, &source, &target, retry_at)
            .unwrap();
        broker
            .accept_target(
                &app.registration,
                id,
                &source,
                &target,
                authorization,
                retry_at,
            )
            .unwrap();
        assert!(matches!(
            broker
                .target_failed(&app.registration, id, &source, &target, retry_at)
                .unwrap()
                .kind,
            BrokerEventKind::RolledBackToSource { .. }
        ));
        assert!(broker.session(id).unwrap().source_live);
    }

    #[test]
    fn capture_authorization_is_fresh_one_shot_and_exactly_bound() {
        let now = Instant::now();
        let app = FakeCallingApp::verified("dev.tilo.calls", "work");
        let impostor = FakeCallingApp::verified("dev.other.calls", "work");
        let source = device("laptop");
        let target = device("tablet");
        let mut broker = RealtimeSessionBroker::new();
        let id = broker
            .register(
                &app.registration,
                source.clone(),
                FakeCallingApp::intent(FakeCallingApp::audio_video()),
                now,
            )
            .unwrap();
        broker
            .propose_target(&app.registration, id, &source, target.clone(), now)
            .unwrap();
        let authorization = broker
            .authorize_target_capture(&app.registration, id, &source, &target, now)
            .unwrap();
        assert_eq!(
            broker.accept_target(
                &impostor.registration,
                id,
                &source,
                &target,
                authorization,
                now,
            ),
            Err(RealtimeSessionError::BindingMismatch)
        );
        broker
            .accept_target(&app.registration, id, &source, &target, authorization, now)
            .unwrap();
        assert_eq!(
            broker.accept_target(&app.registration, id, &source, &target, authorization, now,),
            Err(RealtimeSessionError::InvalidTransition)
        );
    }

    #[test]
    fn restart_scoped_ids_and_exact_bindings_reject_stale_or_foreign_callers() {
        let now = Instant::now();
        let app = FakeCallingApp::verified("dev.tilo.calls", "work");
        let wrong_workspace = FakeCallingApp::verified("dev.tilo.calls", "personal");
        let source = device("laptop");
        let mut first = RealtimeSessionBroker::new();
        let id = first
            .register(
                &app.registration,
                source.clone(),
                FakeCallingApp::intent(FakeCallingApp::audio()),
                now,
            )
            .unwrap();
        assert_eq!(
            first.propose_target(
                &wrong_workspace.registration,
                id,
                &source,
                device("tablet"),
                now,
            ),
            Err(RealtimeSessionError::BindingMismatch)
        );
        let mut restarted = RealtimeSessionBroker::new();
        assert_ne!(first.instance_id(), restarted.instance_id());
        assert_eq!(
            restarted.propose_target(&app.registration, id, &source, device("tablet"), now,),
            Err(RealtimeSessionError::UnknownSession)
        );
    }

    #[test]
    fn registry_is_bounded_and_ttl_forgets_without_stopping_live_source() {
        let now = Instant::now();
        let app = FakeCallingApp::verified("dev.tilo.calls", "work");
        let mut broker =
            RealtimeSessionBroker::with_limits(1, Duration::from_secs(2), Duration::from_secs(1));
        let id = broker
            .register(
                &app.registration,
                device("laptop"),
                FakeCallingApp::intent(FakeCallingApp::audio()),
                now,
            )
            .unwrap();
        assert_eq!(
            broker.register(
                &app.registration,
                device("phone"),
                FakeCallingApp::intent(FakeCallingApp::audio()),
                now,
            ),
            Err(RealtimeSessionError::RegistryFull)
        );
        assert_eq!(
            broker.tick(now + Duration::from_secs(3)),
            vec![BrokerEvent {
                session_id: id,
                kind: BrokerEventKind::ForgottenLive {
                    live_device: device("laptop"),
                },
            }]
        );
        assert!(broker.session(id).is_none());
    }

    #[test]
    fn media_actions_follow_mode_and_only_the_live_endpoint() {
        let now = Instant::now();
        let app = FakeCallingApp::verified("dev.tilo.calls", "work");
        let source = device("laptop");
        let mut broker = RealtimeSessionBroker::new();
        let id = broker
            .register(
                &app.registration,
                source.clone(),
                FakeCallingApp::intent(FakeCallingApp::audio()),
                now,
            )
            .unwrap();
        assert_eq!(
            broker.apply_media_action(
                &app.registration,
                id,
                &source,
                RealtimeMediaAction::SetCameraMuted(true),
                now,
            ),
            Err(RealtimeSessionError::UnsupportedMediaAction)
        );
        assert_eq!(
            broker.apply_media_action(
                &app.registration,
                id,
                &device("tablet"),
                RealtimeMediaAction::SetMicrophoneMuted(true),
                now,
            ),
            Err(RealtimeSessionError::BindingMismatch)
        );
        assert!(
            broker
                .apply_media_action(
                    &app.registration,
                    id,
                    &source,
                    RealtimeMediaAction::SetMicrophoneMuted(true),
                    now,
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn media_facets_are_independent_and_share_audio_never_implies_microphone() {
        assert_eq!(
            RealtimeMediaSet::new([]),
            Err(RealtimeSessionError::InvalidMediaSet)
        );
        assert_eq!(
            RealtimeMediaSet::new([RealtimeMediaFacet::ShareAudio]),
            Err(RealtimeSessionError::InvalidMediaSet)
        );
        let share = RealtimeMediaSet::new([
            RealtimeMediaFacet::ScreenShare,
            RealtimeMediaFacet::ShareAudio,
        ])
        .unwrap();
        assert!(!share.microphone());
        assert!(share.screen_share() && share.share_audio());

        let now = Instant::now();
        let app = FakeCallingApp::verified("dev.tilo.calls", "work");
        let source = device("laptop");
        let mut broker = RealtimeSessionBroker::new();
        let id = broker
            .register(
                &app.registration,
                source.clone(),
                FakeCallingApp::intent(share),
                now,
            )
            .unwrap();
        assert_eq!(
            broker.apply_media_action(
                &app.registration,
                id,
                &source,
                RealtimeMediaAction::SetMicrophoneMuted(true),
                now,
            ),
            Err(RealtimeSessionError::UnsupportedMediaAction)
        );
        assert!(
            broker
                .apply_media_action(
                    &app.registration,
                    id,
                    &source,
                    RealtimeMediaAction::SetShareAudioMuted(true),
                    now,
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn lock_hooks_rollback_pending_target_but_end_a_locked_live_endpoint() {
        let now = Instant::now();
        let app = FakeCallingApp::verified("dev.tilo.calls", "work");
        let source = device("laptop");
        let target = device("tablet");
        let mut broker = RealtimeSessionBroker::new();
        let id = broker
            .register(
                &app.registration,
                source.clone(),
                FakeCallingApp::intent(
                    RealtimeMediaSet::new([
                        RealtimeMediaFacet::ScreenShare,
                        RealtimeMediaFacet::ShareAudio,
                    ])
                    .unwrap(),
                ),
                now,
            )
            .unwrap();
        broker
            .propose_target(&app.registration, id, &source, target.clone(), now)
            .unwrap();
        let target_events = broker.device_locked(&target, now);
        assert!(matches!(
            target_events.first().map(|event| &event.kind),
            Some(BrokerEventKind::RolledBackToSource { .. })
        ));
        assert!(broker.session(id).unwrap().source_live);
        let source_events = broker.device_locked(&source, now);
        assert!(matches!(
            source_events.first().map(|event| &event.kind),
            Some(BrokerEventKind::Ended { .. })
        ));
        assert!(broker.session(id).is_none());
    }

    #[test]
    fn semantic_resume_state_rejects_urls_and_bearer_shaped_queries() {
        assert_eq!(
            SemanticRoomLocator::new("meet", "https://meet.invalid/room"),
            Err(RealtimeSessionError::InvalidRoomLocator)
        );
        assert_eq!(
            SemanticRoomLocator::new("meet", "room?token=secret"),
            Err(RealtimeSessionError::InvalidRoomLocator)
        );
        let intent = RealtimeResumeIntent {
            room: SemanticRoomLocator::new("meet", "safe-room").unwrap(),
            semantic_position: Some("https://unsafe.invalid/resume".into()),
            media: FakeCallingApp::audio(),
        };
        assert_eq!(
            intent.validate(),
            Err(RealtimeSessionError::InvalidResumeIntent)
        );
    }
}
