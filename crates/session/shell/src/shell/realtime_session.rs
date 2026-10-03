//! Framework-owned continuity for active calls, meetings, and live rooms.
//!
//! This module is deliberately transport agnostic. It coordinates semantic
//! room resumption and cutover, but never represents a `PipeWire` node, portal
//! object, file descriptor, or capture stream. Those are target-local objects
//! that must be freshly authorized after an accept.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};
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

    #[must_use]
    pub const fn fabric_id(self) -> RealtimeFabricSessionId {
        RealtimeFabricSessionId {
            broker: self.broker.0,
            nonce: self.nonce,
        }
    }
}

/// Restart-scoped opaque identity used on the certified device fabric. It
/// contains no app token, capture handle, or transport locator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RealtimeFabricSessionId {
    pub broker: [u8; 16],
    pub nonce: [u8; 16],
}

impl RealtimeFabricSessionId {
    #[must_use]
    pub fn valid(self) -> bool {
        self.broker != [0; 16] && self.nonce != [0; 16]
    }

    pub(crate) const fn into_broker_id(self) -> RealtimeSessionId {
        RealtimeSessionId {
            broker: BrokerInstanceId(self.broker),
            nonce: self.nonce,
        }
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

    #[must_use]
    pub const fn fabric_bits(self) -> u8 {
        self.bits
    }

    /// # Errors
    ///
    /// Returns an error for unknown bits or an invalid facet combination.
    pub fn from_fabric_bits(bits: u8) -> Result<Self, RealtimeSessionError> {
        if bits & !0b1111 != 0 {
            return Err(RealtimeSessionError::InvalidMediaSet);
        }
        let mut facets = Vec::with_capacity(4);
        if bits & (1 << 0) != 0 {
            facets.push(RealtimeMediaFacet::Microphone);
        }
        if bits & (1 << 1) != 0 {
            facets.push(RealtimeMediaFacet::Camera);
        }
        if bits & (1 << 2) != 0 {
            facets.push(RealtimeMediaFacet::ScreenShare);
        }
        if bits & (1 << 3) != 0 {
            facets.push(RealtimeMediaFacet::ShareAudio);
        }
        Self::new(facets)
    }
}

/// Bounded semantic offer carried over the personal fabric. Raw URLs,
/// capture grants and process-local media handles have no representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeFabricOffer {
    pub session_id: RealtimeFabricSessionId,
    pub app_id: String,
    pub workspace_id: String,
    pub provider: String,
    pub room_id: String,
    pub semantic_position: Option<String>,
    pub media_bits: u8,
    #[serde(default)]
    pub controls: RealtimeControlState,
    pub expires_unix_ms: u64,
}

/// Bounded semantic control state that is safe to resume on another device.
/// It contains no transport, capture, or hardware identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each control is an independent user toggle carried on the wire as-is"
)]
pub struct RealtimeControlState {
    pub microphone_muted: bool,
    pub camera_muted: bool,
    pub screen_share_paused: bool,
    pub share_audio_muted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealtimeFabricSignal {
    Offer(RealtimeFabricOffer),
    Accept(RealtimeFabricSessionId),
    MediaReady(RealtimeFabricSessionId),
    StopSource(RealtimeFabricSessionId),
    Cancel(RealtimeFabricSessionId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeFabricEgress {
    pub target: DeviceId,
    pub signal: RealtimeFabricSignal,
}

/// A newly admitted remote offer for the target-local application bridge.
/// The authenticated envelope endpoints are retained alongside the semantic
/// projection so IPC routing never has to reconstruct device authority from
/// application-provided data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedRealtimeFabricOffer {
    pub offer: RealtimeFabricOffer,
    pub source: DeviceId,
    pub target: DeviceId,
}

/// Narrow thread-safe seam between certified transport and the app-facing
/// realtime broker. Implementations own local consent/UI routing; the fabric
/// runtime only authenticates endpoints and transports semantic signals.
pub trait RealtimeFabricAdapter: Send + Sync {
    fn receive_authenticated(
        &self,
        source: &DeviceId,
        target: &DeviceId,
        signal: RealtimeFabricSignal,
        now: Instant,
        now_unix_ms: u64,
    ) -> bool;

    fn peer_offline(&self, peer: &DeviceId, now: Instant);

    fn tick(&self, now: Instant);

    fn drain_egress(&self) -> Vec<RealtimeFabricEgress>;
}

pub type SharedRealtimeFabricAdapter = Arc<dyn RealtimeFabricAdapter>;

struct BrokerFabricAdapterState {
    broker: Arc<Mutex<RealtimeSessionBroker>>,
    egress: VecDeque<RealtimeFabricEgress>,
    imported_offers: VecDeque<ImportedRealtimeFabricOffer>,
    authorizations: BTreeMap<RealtimeFabricSessionId, CaptureAuthorizationId>,
    cutovers: VecDeque<CutoverDirective>,
    lifecycle_events: VecDeque<BrokerEvent>,
}

/// Default bounded adapter for importing certified fabric signals into a
/// shared shell realtime broker. App-facing code supplies target-local
/// capture consent by calling [`Self::accept_imported`] and never exposes the
/// resulting authorization on the wire.
pub struct BrokerRealtimeFabricAdapter {
    state: Mutex<BrokerFabricAdapterState>,
    app_allowed: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl BrokerRealtimeFabricAdapter {
    #[must_use]
    pub fn new(broker: Arc<Mutex<RealtimeSessionBroker>>) -> Self {
        Self::with_app_verifier(broker, |app_id| {
            crate::shell::launcher::installed_apps()
                .is_ok_and(|apps| apps.iter().any(|installed| installed.package_id == app_id))
        })
    }

    #[must_use]
    pub fn with_app_verifier(
        broker: Arc<Mutex<RealtimeSessionBroker>>,
        verifier: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Mutex::new(BrokerFabricAdapterState {
                broker,
                egress: VecDeque::new(),
                imported_offers: VecDeque::new(),
                authorizations: BTreeMap::new(),
                cutovers: VecDeque::new(),
                lifecycle_events: VecDeque::new(),
            }),
            app_allowed: Arc::new(verifier),
        }
    }

    /// Queue an offer only for the exact target already proposed in the
    /// source broker.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, is not awaiting the
    /// exact target, or broker state is unavailable.
    pub fn offer_to(
        &self,
        session_id: RealtimeSessionId,
        target: &DeviceId,
        expires_unix_ms: u64,
    ) -> Result<(), RealtimeSessionError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?;
        let broker = Arc::clone(&state.broker);
        drop(state);
        let (offer, expected) = {
            let broker = broker
                .lock()
                .map_err(|_| RealtimeSessionError::InvalidTransition)?;
            let view = broker
                .session(session_id)
                .ok_or(RealtimeSessionError::UnknownSession)?;
            (
                broker.fabric_offer(session_id, expires_unix_ms)?,
                view.target,
            )
        };
        if expected.as_ref() != Some(target) {
            return Err(RealtimeSessionError::BindingMismatch);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?;
        push_fabric_egress(
            &mut state.egress,
            RealtimeFabricEgress {
                target: target.clone(),
                signal: RealtimeFabricSignal::Offer(offer),
            },
        );
        drop(state);
        Ok(())
    }

    /// Record target-local explicit consent and acknowledge the exact source.
    /// The broker authorization remains process-local.
    ///
    /// # Errors
    ///
    /// Returns an error when the imported session/app/endpoints do not match,
    /// capture authorization fails, or broker state is unavailable.
    pub fn accept_imported(
        &self,
        session_id: RealtimeFabricSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<(), RealtimeSessionError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?;
        let broker = Arc::clone(&state.broker);
        drop(state);
        let id = session_id.into_broker_id();
        let view = broker
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?
            .session(id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        let app = VerifiedAppRegistration::new_verified(view.app_id, view.workspace_id)?;
        let authorization = {
            let mut broker = broker
                .lock()
                .map_err(|_| RealtimeSessionError::InvalidTransition)?;
            let authorization = broker.authorize_target_capture(&app, id, source, target, now)?;
            broker.accept_target(&app, id, source, target, authorization, now)?;
            authorization
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?;
        state.authorizations.insert(session_id, authorization);
        push_fabric_egress(
            &mut state.egress,
            RealtimeFabricEgress {
                target: source.clone(),
                signal: RealtimeFabricSignal::Accept(session_id),
            },
        );
        drop(state);
        Ok(())
    }

    /// Confirm that target-local capture streams are live, then tell only the
    /// exact source. The source remains authoritative until it replies with
    /// `StopSource`.
    ///
    /// # Errors
    ///
    /// Returns an error when fresh target-local authorization is absent, the
    /// exact session/endpoints do not match, or broker state is unavailable.
    pub fn imported_media_ready(
        &self,
        session_id: RealtimeFabricSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<(), RealtimeSessionError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?;
        let authorization = state
            .authorizations
            .remove(&session_id)
            .ok_or(RealtimeSessionError::CaptureAuthorizationRequired)?;
        let broker = Arc::clone(&state.broker);
        drop(state);
        let id = session_id.into_broker_id();
        let view = broker
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?
            .session(id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        let app = VerifiedAppRegistration::new_verified(view.app_id, view.workspace_id)?;
        broker
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?
            .media_ready(&app, id, source, target, authorization, now)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?;
        push_fabric_egress(
            &mut state.egress,
            RealtimeFabricEgress {
                target: source.clone(),
                signal: RealtimeFabricSignal::MediaReady(session_id),
            },
        );
        drop(state);
        Ok(())
    }

    /// Roll back target-local setup and notify only the authenticated source.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact imported session/endpoints do not
    /// match, rollback is no longer valid, or broker state is unavailable.
    pub fn fail_imported(
        &self,
        session_id: RealtimeFabricSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<(), RealtimeSessionError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?;
        let broker = Arc::clone(&state.broker);
        drop(state);
        let id = session_id.into_broker_id();
        let view = broker
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?
            .session(id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        let app = VerifiedAppRegistration::new_verified(view.app_id, view.workspace_id)?;
        broker
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?
            .target_failed(&app, id, source, target, now)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RealtimeSessionError::InvalidTransition)?;
        state.authorizations.remove(&session_id);
        push_fabric_egress(
            &mut state.egress,
            RealtimeFabricEgress {
                target: source.clone(),
                signal: RealtimeFabricSignal::Cancel(session_id),
            },
        );
        drop(state);
        Ok(())
    }

    pub fn drain_cutovers(&self) -> Vec<CutoverDirective> {
        self.state.lock().map_or_else(
            |_| Vec::new(),
            |mut state| state.cutovers.drain(..).collect(),
        )
    }

    /// Drain newly imported offers after the certified transport has admitted
    /// them. The production application bridge turns each item into an opaque
    /// local session token and delivers it only to a matching certified app.
    pub fn drain_imported_offers(&self) -> Vec<ImportedRealtimeFabricOffer> {
        self.state.lock().map_or_else(
            |_| Vec::new(),
            |mut state| state.imported_offers.drain(..).collect(),
        )
    }

    pub fn drain_lifecycle_events(&self) -> Vec<BrokerEvent> {
        self.state.lock().map_or_else(
            |_| Vec::new(),
            |mut state| state.lifecycle_events.drain(..).collect(),
        )
    }
}

fn push_fabric_egress(queue: &mut VecDeque<RealtimeFabricEgress>, event: RealtimeFabricEgress) {
    if queue.len() == MAX_REALTIME_SESSIONS {
        queue.pop_front();
    }
    queue.push_back(event);
}

fn push_broker_events(
    queue: &mut VecDeque<BrokerEvent>,
    events: impl IntoIterator<Item = BrokerEvent>,
) {
    for event in events {
        if queue.len() == MAX_REALTIME_SESSIONS {
            queue.pop_front();
        }
        queue.push_back(event);
    }
}

impl RealtimeFabricAdapter for BrokerRealtimeFabricAdapter {
    fn receive_authenticated(
        &self,
        source: &DeviceId,
        target: &DeviceId,
        signal: RealtimeFabricSignal,
        now: Instant,
        now_unix_ms: u64,
    ) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        match signal {
            RealtimeFabricSignal::Offer(offer) => {
                if !(self.app_allowed)(&offer.app_id) {
                    return false;
                }
                let imported = state.broker.lock().is_ok_and(|mut broker| {
                    broker
                        .import_fabric_offer(
                            &offer,
                            source.clone(),
                            target.clone(),
                            now,
                            now_unix_ms,
                        )
                        .unwrap_or(false)
                });
                if imported {
                    if state.imported_offers.len() == MAX_REALTIME_SESSIONS {
                        state.imported_offers.pop_front();
                    }
                    state
                        .imported_offers
                        .push_back(ImportedRealtimeFabricOffer {
                            offer,
                            source: source.clone(),
                            target: target.clone(),
                        });
                }
                imported
            }
            RealtimeFabricSignal::Accept(id) => state.broker.lock().is_ok_and(|mut broker| {
                broker
                    .remote_target_accepted(id, target, source, now)
                    .is_ok()
            }),
            RealtimeFabricSignal::MediaReady(id) => {
                let directive =
                    state.broker.lock().ok().and_then(|mut broker| {
                        broker.remote_media_ready(id, target, source, now).ok()
                    });
                let Some(directive) = directive else {
                    return false;
                };
                push_fabric_egress(
                    &mut state.egress,
                    RealtimeFabricEgress {
                        target: source.clone(),
                        signal: RealtimeFabricSignal::StopSource(id),
                    },
                );
                if state.cutovers.len() == MAX_REALTIME_SESSIONS {
                    state.cutovers.pop_front();
                }
                state.cutovers.push_back(directive);
                true
            }
            RealtimeFabricSignal::StopSource(id) => state.broker.lock().is_ok_and(|mut broker| {
                broker
                    .confirm_source_stopped(id, source, target, now)
                    .is_ok()
            }),
            RealtimeFabricSignal::Cancel(id) => {
                let event = state.broker.lock().ok().and_then(|mut broker| {
                    let broker_id = id.into_broker_id();
                    let before = broker.session(broker_id)?;
                    broker.cancel_fabric_handoff(id, source, target, now).ok()?;
                    let kind = if broker.session(broker_id).is_some() {
                        BrokerEventKind::RolledBackToSource {
                            target: before.target?,
                        }
                    } else {
                        BrokerEventKind::Ended {
                            live_device: before.source,
                        }
                    };
                    Some(BrokerEvent {
                        session_id: broker_id,
                        kind,
                    })
                });
                let cancelled = event.is_some();
                if cancelled {
                    state.authorizations.remove(&id);
                    push_broker_events(&mut state.lifecycle_events, event);
                }
                cancelled
            }
        }
    }

    fn peer_offline(&self, peer: &DeviceId, now: Instant) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let broker = Arc::clone(&state.broker);
        let Ok(mut broker) = broker.lock() else {
            return;
        };
        let events = broker.device_lost(peer, now);
        let live_authorizations = broker.pending_fabric_authorizations();
        drop(broker);
        push_broker_events(&mut state.lifecycle_events, events);
        state
            .authorizations
            .retain(|session_id, _| live_authorizations.contains_key(session_id));
    }

    fn tick(&self, now: Instant) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let broker = Arc::clone(&state.broker);
        let Ok(mut broker) = broker.lock() else {
            return;
        };
        let events = broker.tick(now);
        let live_authorizations = broker.pending_fabric_authorizations();
        drop(broker);
        push_broker_events(&mut state.lifecycle_events, events);
        state
            .authorizations
            .retain(|session_id, _| live_authorizations.contains_key(session_id));
    }

    fn drain_egress(&self) -> Vec<RealtimeFabricEgress> {
        self.state
            .lock()
            .map_or_else(|_| Vec::new(), |mut state| state.egress.drain(..).collect())
    }
}

impl RealtimeFabricOffer {
    #[must_use]
    pub fn valid_at(&self, now_unix_ms: u64) -> bool {
        self.session_id.valid()
            && valid_identifier(&self.app_id, 256)
            && valid_identifier(&self.workspace_id, 128)
            && SemanticRoomLocator::new(&self.provider, &self.room_id).is_ok()
            && RealtimeMediaSet::from_fabric_bits(self.media_bits).is_ok()
            && self.semantic_position.as_ref().is_none_or(|position| {
                !position.is_empty()
                    && position.len() <= 512
                    && !position.chars().any(char::is_control)
                    && !position.contains("://")
                    && !position.contains('?')
            })
            && self.expires_unix_ms > now_unix_ms
            && self.expires_unix_ms.saturating_sub(now_unix_ms) <= 30_000
    }

    /// # Errors
    ///
    /// Returns an error if semantic fabric metadata cannot be reconstructed
    /// into the transport-neutral broker model.
    pub fn intent(&self) -> Result<RealtimeResumeIntent, RealtimeSessionError> {
        let intent = RealtimeResumeIntent {
            room: SemanticRoomLocator::new(self.provider.clone(), self.room_id.clone())?,
            semantic_position: self.semantic_position.clone(),
            media: RealtimeMediaSet::from_fabric_bits(self.media_bits)?,
            controls: self.controls,
        };
        intent.validate()?;
        Ok(intent)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeResumeIntent {
    pub room: SemanticRoomLocator,
    /// App-owned semantic state such as "stage" or "breakout-2". It must not
    /// contain a URL, access token, or process-local media identifier.
    pub semantic_position: Option<String>,
    pub media: RealtimeMediaSet,
    pub controls: RealtimeControlState,
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
    AwaitingSourceStop,
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
    AwaitingRemoteMedia {
        target: DeviceId,
        deadline: Instant,
    },
    AwaitingSourceStop {
        target: DeviceId,
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
            Self::AwaitingMedia { .. } | Self::AwaitingRemoteMedia { .. } => {
                RealtimeSessionPhase::AwaitingTargetMedia
            }
            Self::AwaitingSourceStop { .. } => RealtimeSessionPhase::AwaitingSourceStop,
            Self::TargetLive { .. } => RealtimeSessionPhase::TargetLive,
        }
    }

    fn target(&self) -> Option<&DeviceId> {
        match self {
            Self::SourceLive => None,
            Self::AwaitingAccept { target, .. }
            | Self::AwaitingMedia { target, .. }
            | Self::AwaitingRemoteMedia { target, .. }
            | Self::AwaitingSourceStop { target, .. }
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
    imported: bool,
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
                imported: false,
            },
        );
        Ok(id)
    }

    /// Import one metadata-only offer authored by an authenticated certified
    /// source. Repeating the identical offer is idempotent; a conflicting
    /// offer with the same restart-scoped id fails closed.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid metadata, capacity, or binding conflicts.
    pub fn import_fabric_offer(
        &mut self,
        offer: &RealtimeFabricOffer,
        source: DeviceId,
        target: DeviceId,
        now: Instant,
        now_unix_ms: u64,
    ) -> Result<bool, RealtimeSessionError> {
        if source == target || !offer.valid_at(now_unix_ms) {
            return Err(RealtimeSessionError::InvalidResumeIntent);
        }
        let id = offer.session_id.into_broker_id();
        let app = VerifiedAppRegistration::new_verified(
            offer.app_id.clone(),
            offer.workspace_id.clone(),
        )?;
        let intent = offer.intent()?;
        if let Some(existing) = self.sessions.get(&id) {
            let same = existing.imported
                && existing.app == app
                && existing.source == source
                && existing.intent == intent
                && existing.state.target() == Some(&target);
            return if same {
                Ok(false)
            } else {
                Err(RealtimeSessionError::BindingMismatch)
            };
        }
        if self.sessions.len() >= self.max_sessions {
            return Err(RealtimeSessionError::RegistryFull);
        }
        self.sessions.insert(
            id,
            SessionEntry {
                app,
                source,
                intent,
                state: SessionState::AwaitingAccept {
                    target,
                    deadline: deadline(now, self.cutover_timeout),
                },
                last_activity: now,
                last_action: None,
                imported: true,
            },
        );
        Ok(true)
    }

    /// Convert a target shell's authenticated acceptance into a source-side
    /// transition without importing its local capture authorization.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale or mismatched target/session binding.
    pub fn remote_target_accepted(
        &mut self,
        session_id: RealtimeFabricSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<bool, RealtimeSessionError> {
        let id = session_id.into_broker_id();
        let cutover_timeout = self.cutover_timeout;
        let entry = self
            .sessions
            .get_mut(&id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        if entry.imported || &entry.source != source {
            return Err(RealtimeSessionError::BindingMismatch);
        }
        match &entry.state {
            SessionState::AwaitingAccept {
                target: expected,
                deadline: expires,
            } if expected == target && now <= *expires => {
                entry.state = SessionState::AwaitingRemoteMedia {
                    target: target.clone(),
                    deadline: now.checked_add(cutover_timeout).unwrap_or(now),
                };
                entry.last_activity = now;
                Ok(true)
            }
            SessionState::AwaitingRemoteMedia {
                target: expected, ..
            } if expected == target => Ok(false),
            _ => Err(RealtimeSessionError::InvalidTransition),
        }
    }

    /// Authenticate target media readiness at the source. This is the only
    /// remote edge which returns `stop_source = true`.
    ///
    /// # Errors
    ///
    /// Returns an error for replay, expiry, or binding mismatch.
    pub fn remote_media_ready(
        &mut self,
        session_id: RealtimeFabricSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<CutoverDirective, RealtimeSessionError> {
        let id = session_id.into_broker_id();
        let entry = self
            .sessions
            .get_mut(&id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        if entry.imported || &entry.source != source {
            return Err(RealtimeSessionError::BindingMismatch);
        }
        if !matches!(&entry.state, SessionState::AwaitingRemoteMedia { target: expected, deadline } if expected == target && now <= *deadline)
        {
            return Err(RealtimeSessionError::InvalidTransition);
        }
        entry.state = SessionState::TargetLive {
            target: target.clone(),
        };
        entry.last_activity = now;
        Ok(CutoverDirective {
            session_id: id,
            app_id: entry.app.app_id.clone(),
            workspace_id: entry.app.workspace_id.clone(),
            source: source.clone(),
            target: target.clone(),
            stop_source: true,
        })
    }

    /// Commit an imported target only after the source acknowledges that its
    /// endpoint stopped.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown, expired, reordered, or endpoint-
    /// mismatched acknowledgement.
    pub fn confirm_source_stopped(
        &mut self,
        session_id: RealtimeFabricSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<bool, RealtimeSessionError> {
        let id = session_id.into_broker_id();
        let entry = self
            .sessions
            .get_mut(&id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        if !entry.imported || &entry.source != source {
            return Err(RealtimeSessionError::BindingMismatch);
        }
        match &entry.state {
            SessionState::AwaitingSourceStop {
                target: expected,
                deadline,
            } if expected == target && now <= *deadline => {
                entry.state = SessionState::TargetLive {
                    target: target.clone(),
                };
                entry.last_activity = now;
                Ok(true)
            }
            SessionState::TargetLive { target: expected } if expected == target => Ok(false),
            _ => Err(RealtimeSessionError::InvalidTransition),
        }
    }

    /// Cancel an exact in-flight fabric handoff. Imported target offers are
    /// forgotten; source-owned handoffs roll back to their still-live source.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown session, mismatched endpoints, or a
    /// session that is no longer in a cancellable transition.
    pub fn cancel_fabric_handoff(
        &mut self,
        session_id: RealtimeFabricSessionId,
        source: &DeviceId,
        target: &DeviceId,
        now: Instant,
    ) -> Result<bool, RealtimeSessionError> {
        let id = session_id.into_broker_id();
        let entry = self
            .sessions
            .get(&id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        let canonical = &entry.source == source && entry.state.target() == Some(target);
        let reversed = &entry.source == target && entry.state.target() == Some(source);
        if !(canonical || reversed) {
            return Err(RealtimeSessionError::BindingMismatch);
        }
        if entry.imported {
            self.sessions.remove(&id);
            self.revoke_session_authorizations(id);
            return Ok(true);
        }
        let app = entry.app.clone();
        let canonical_source = entry.source.clone();
        let canonical_target = entry
            .state
            .target()
            .cloned()
            .ok_or(RealtimeSessionError::InvalidTransition)?;
        self.target_failed(&app, id, &canonical_source, &canonical_target, now)?;
        Ok(true)
    }

    /// Build the bounded semantic wire offer for a source-owned pending
    /// handoff. No local capture state is copied.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, imported, not awaiting a
    /// target, or contains metadata that cannot be projected safely.
    pub fn fabric_offer(
        &self,
        session_id: RealtimeSessionId,
        expires_unix_ms: u64,
    ) -> Result<RealtimeFabricOffer, RealtimeSessionError> {
        let entry = self
            .sessions
            .get(&session_id)
            .ok_or(RealtimeSessionError::UnknownSession)?;
        if entry.imported || !matches!(entry.state, SessionState::AwaitingAccept { .. }) {
            return Err(RealtimeSessionError::InvalidTransition);
        }
        Ok(RealtimeFabricOffer {
            session_id: session_id.fabric_id(),
            app_id: entry.app.app_id.clone(),
            workspace_id: entry.app.workspace_id.clone(),
            provider: entry.intent.room.provider.clone(),
            room_id: entry.intent.room.room_id.clone(),
            semantic_position: entry.intent.semantic_position.clone(),
            media_bits: entry.intent.media.fabric_bits(),
            controls: entry.intent.controls,
            expires_unix_ms,
        })
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
        let cutover_timeout = self.cutover_timeout;
        let entry = self.bound_entry_mut(app, session_id, source)?;
        if !matches!(&entry.state, SessionState::AwaitingMedia { target: expected, authorization, deadline } if expected == target && *authorization == authorization_id && now <= *deadline)
        {
            return Err(RealtimeSessionError::InvalidTransition);
        }
        let imported = entry.imported;
        entry.state = if imported {
            SessionState::AwaitingSourceStop {
                target: target.clone(),
                deadline: deadline(now, cutover_timeout),
            }
        } else {
            SessionState::TargetLive {
                target: target.clone(),
            }
        };
        entry.last_activity = now;
        Ok(CutoverDirective {
            session_id,
            app_id: app.app_id.clone(),
            workspace_id: app.workspace_id.clone(),
            source: source.clone(),
            target: target.clone(),
            stop_source: !imported,
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
                }
                | SessionState::AwaitingRemoteMedia { target, deadline }
                | SessionState::AwaitingSourceStop { target, deadline }
                    if now > *deadline =>
                {
                    Some(target.clone())
                }
                _ => None,
            };
            if let Some(target) = timed_out_target {
                if entry.imported {
                    let source = entry.source.clone();
                    self.sessions.remove(&id);
                    self.revoke_session_authorizations(id);
                    events.push(BrokerEvent {
                        session_id: id,
                        kind: BrokerEventKind::ForgottenLive {
                            live_device: source,
                        },
                    });
                    continue;
                }
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

    fn pending_fabric_authorizations(
        &self,
    ) -> BTreeMap<RealtimeFabricSessionId, CaptureAuthorizationId> {
        self.sessions
            .iter()
            .filter_map(|(id, entry)| match entry.state {
                SessionState::AwaitingMedia { authorization, .. } if entry.imported => {
                    Some((id.fabric_id(), authorization))
                }
                _ => None,
            })
            .collect()
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
                | SessionState::AwaitingRemoteMedia { target, .. }
                | SessionState::AwaitingSourceStop { target, .. }
                    if target == device =>
                {
                    Some(target.clone())
                }
                _ => None,
            };
            if let Some(target) = pending_target {
                if entry.imported {
                    let source = entry.source.clone();
                    self.sessions.remove(&id);
                    self.revoke_session_authorizations(id);
                    events.push(BrokerEvent {
                        session_id: id,
                        kind: BrokerEventKind::ForgottenLive {
                            live_device: source,
                        },
                    });
                    continue;
                }
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
        | SessionState::AwaitingMedia { .. }
        | SessionState::AwaitingRemoteMedia { .. }
        | SessionState::AwaitingSourceStop { .. } => &entry.source,
        SessionState::TargetLive { target } => target,
    }
}

fn random_nonzero() -> [u8; 16] {
    let mut value = [0; 16];
    while value == [0; 16] {
        UnwrapErr(SysRng).fill_bytes(&mut value);
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
                controls: RealtimeControlState::default(),
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn cutover_waits_for_accept_and_media_ready_and_rolls_back_safely() {
        // Phase `cutover_requires_accept_then_media_ready_and_keeps_source_live_until_ready`.
        {
            let now = Instant::now();
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
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

        // Phase `timeout_and_target_loss_roll_back_without_stopping_source`.
        {
            let now = Instant::now();
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
            let source = device("laptop");
            let target = device("tablet");
            let mut broker = RealtimeSessionBroker::with_limits(
                4,
                Duration::from_secs(60),
                Duration::from_secs(2),
            );
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

        // Phase `lock_hooks_rollback_pending_target_but_end_a_locked_live_endpoint`.
        {
            let now = Instant::now();
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn capture_authority_is_fresh_exactly_bound_and_bounded() {
        // Phase `capture_authorization_is_fresh_one_shot_and_exactly_bound`.
        {
            let now = Instant::now();
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
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

        // Phase `restart_scoped_ids_and_exact_bindings_reject_stale_or_foreign_callers`.
        {
            let now = Instant::now();
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
            let wrong_workspace = FakeCallingApp::verified("so.tilo.calls", "personal");
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

        // Phase `registry_is_bounded_and_ttl_forgets_without_stopping_live_source`.
        {
            let now = Instant::now();
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
            let mut broker = RealtimeSessionBroker::with_limits(
                1,
                Duration::from_secs(2),
                Duration::from_secs(1),
            );
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_actions_and_facets_follow_the_live_endpoint_and_mode() {
        // Phase `media_actions_follow_mode_and_only_the_live_endpoint`.
        {
            let now = Instant::now();
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
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

        // Phase `media_facets_are_independent_and_share_audio_never_implies_microphone`.
        {
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
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
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

        // Phase `semantic_resume_state_rejects_urls_and_bearer_shaped_queries`.
        {
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
                controls: RealtimeControlState::default(),
            };
            assert_eq!(
                intent.validate(),
                Err(RealtimeSessionError::InvalidResumeIntent)
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn fabric_handoff_cuts_over_only_to_the_authenticated_ready_peer() {
        // Phase `two_peer_fabric_cutover_stops_source_only_after_authenticated_ready`.
        {
            let now = Instant::now();
            let source = device("laptop");
            let target = device("tablet");
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
            let mut origin = RealtimeSessionBroker::new();
            let id = origin
                .register(
                    &app.registration,
                    source.clone(),
                    FakeCallingApp::intent(FakeCallingApp::audio()),
                    now,
                )
                .unwrap();
            origin
                .propose_target(&app.registration, id, &source, target.clone(), now)
                .unwrap();
            let offer = origin.fabric_offer(id, 20_000).unwrap();

            let mut receiver = RealtimeSessionBroker::new();
            assert!(
                receiver
                    .import_fabric_offer(&offer, source.clone(), target.clone(), now, 10_000)
                    .unwrap()
            );
            assert!(
                !receiver
                    .import_fabric_offer(&offer, source.clone(), target.clone(), now, 10_000)
                    .unwrap()
            );
            let remote_id = offer.session_id.into_broker_id();
            let authorization = receiver
                .authorize_target_capture(&app.registration, remote_id, &source, &target, now)
                .unwrap();
            receiver
                .accept_target(
                    &app.registration,
                    remote_id,
                    &source,
                    &target,
                    authorization,
                    now,
                )
                .unwrap();
            assert!(origin.session(id).unwrap().source_live);
            assert!(
                origin
                    .remote_target_accepted(offer.session_id, &source, &target, now)
                    .unwrap()
            );
            assert!(
                !origin
                    .remote_target_accepted(offer.session_id, &source, &target, now)
                    .unwrap()
            );

            let target_ready = receiver
                .media_ready(
                    &app.registration,
                    remote_id,
                    &source,
                    &target,
                    authorization,
                    now,
                )
                .unwrap();
            assert!(!target_ready.stop_source);
            assert!(origin.session(id).unwrap().source_live);
            let stop = origin
                .remote_media_ready(offer.session_id, &source, &target, now)
                .unwrap();
            assert!(stop.stop_source);
            assert!(!origin.session(id).unwrap().source_live);
            assert!(
                receiver
                    .confirm_source_stopped(offer.session_id, &source, &target, now)
                    .unwrap()
            );
            assert_eq!(
                receiver.session(remote_id).unwrap().phase,
                RealtimeSessionPhase::TargetLive
            );
        }

        // Phase `fabric_handoff_rejects_wrong_peer_reorder_and_offline_pending_offer`.
        {
            let now = Instant::now();
            let source = device("laptop");
            let target = device("tablet");
            let attacker = device("attacker");
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
            let mut origin = RealtimeSessionBroker::new();
            let id = origin
                .register(
                    &app.registration,
                    source.clone(),
                    FakeCallingApp::intent(FakeCallingApp::audio()),
                    now,
                )
                .unwrap();
            origin
                .propose_target(&app.registration, id, &source, target.clone(), now)
                .unwrap();
            let offer = origin.fabric_offer(id, 20_000).unwrap();
            assert_eq!(
                origin.remote_media_ready(offer.session_id, &source, &target, now),
                Err(RealtimeSessionError::InvalidTransition)
            );
            assert_eq!(
                origin.remote_target_accepted(offer.session_id, &source, &attacker, now),
                Err(RealtimeSessionError::InvalidTransition)
            );
            assert!(origin.session(id).unwrap().source_live);

            let mut receiver = RealtimeSessionBroker::new();
            receiver
                .import_fabric_offer(&offer, source.clone(), target, now, 10_000)
                .unwrap();
            let events = receiver.device_lost(&source, now);
            assert_eq!(events.len(), 1);
            assert!(
                receiver
                    .session(offer.session_id.into_broker_id())
                    .is_none()
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn certified_adapter_routes_cancels_and_times_out_exactly() {
        // Phase `certified_adapter_routes_two_peer_handshake_and_cutover_exactly_once`.
        {
            let now = Instant::now();
            let source = device("laptop");
            let target = device("tablet");
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
            let source_broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
            let target_broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
            let source_adapter =
                BrokerRealtimeFabricAdapter::with_app_verifier(Arc::clone(&source_broker), |_| {
                    true
                });
            let target_adapter =
                BrokerRealtimeFabricAdapter::with_app_verifier(Arc::clone(&target_broker), |_| {
                    true
                });
            let mut intent = FakeCallingApp::intent(FakeCallingApp::audio());
            intent.controls = RealtimeControlState {
                microphone_muted: true,
                camera_muted: false,
                screen_share_paused: true,
                share_audio_muted: true,
            };
            let id = source_broker
                .lock()
                .unwrap()
                .register(&app.registration, source.clone(), intent, now)
                .unwrap();
            source_broker
                .lock()
                .unwrap()
                .propose_target(&app.registration, id, &source, target.clone(), now)
                .unwrap();
            source_adapter.offer_to(id, &target, 20_000).unwrap();
            let offer = source_adapter.drain_egress().pop().unwrap();
            let RealtimeFabricSignal::Offer(projection) = offer.signal else {
                panic!("expected offer");
            };
            assert_eq!(
                projection.controls,
                RealtimeControlState {
                    microphone_muted: true,
                    camera_muted: false,
                    screen_share_paused: true,
                    share_audio_muted: true,
                }
            );
            let wire = serde_json::to_string(&projection).unwrap();
            for forbidden in ["pipewire", "device_id", "capture", "credential", "/dev/"] {
                assert!(!wire.to_ascii_lowercase().contains(forbidden));
            }
            let fabric_id = projection.session_id;
            assert!(target_adapter.receive_authenticated(
                &source,
                &target,
                RealtimeFabricSignal::Offer(projection.clone()),
                now,
                10_000,
            ));
            assert_eq!(
                target_adapter.drain_imported_offers(),
                vec![ImportedRealtimeFabricOffer {
                    offer: projection.clone(),
                    source: source.clone(),
                    target: target.clone(),
                }]
            );
            assert!(
                !target_adapter.receive_authenticated(
                    &source,
                    &target,
                    RealtimeFabricSignal::Offer(projection.clone()),
                    now,
                    10_000,
                ),
                "a fresh envelope must not revive the same semantic offer"
            );
            assert!(target_adapter.drain_imported_offers().is_empty());
            assert!(!target_adapter.receive_authenticated(
                &device("attacker"),
                &target,
                RealtimeFabricSignal::Offer(projection),
                now,
                10_000,
            ));
            target_adapter
                .accept_imported(fabric_id, &source, &target, now)
                .unwrap();
            let accepted = target_adapter.drain_egress().pop().unwrap();
            assert_eq!(accepted.target, source);
            assert!(source_adapter.receive_authenticated(
                &target,
                &source,
                accepted.signal,
                now,
                10_000,
            ));
            target_adapter
                .imported_media_ready(fabric_id, &source, &target, now)
                .unwrap();
            let ready = target_adapter.drain_egress().pop().unwrap();
            assert!(source_adapter.receive_authenticated(
                &target,
                &source,
                ready.signal,
                now,
                10_000,
            ));
            let stop = source_adapter.drain_egress().pop().unwrap();
            assert_eq!(stop.target, target);
            assert_eq!(source_adapter.drain_cutovers().len(), 1);
            assert!(target_adapter.receive_authenticated(
                &source,
                &target,
                stop.signal,
                now,
                10_000,
            ));
            assert_eq!(
                target_broker
                    .lock()
                    .unwrap()
                    .session(fabric_id.into_broker_id())
                    .unwrap()
                    .phase,
                RealtimeSessionPhase::TargetLive
            );
        }

        // Phase `adapter_cancel_is_exactly_bound_and_prunes_local_authorizations`.
        {
            let now = Instant::now();
            let source = device("laptop");
            let target = device("tablet");
            let attacker = device("attacker");
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
            let source_broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
            let target_broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
            let source_adapter =
                BrokerRealtimeFabricAdapter::with_app_verifier(Arc::clone(&source_broker), |_| {
                    true
                });
            let target_adapter =
                BrokerRealtimeFabricAdapter::with_app_verifier(Arc::clone(&target_broker), |_| {
                    true
                });
            let id = source_broker
                .lock()
                .unwrap()
                .register(
                    &app.registration,
                    source.clone(),
                    FakeCallingApp::intent(FakeCallingApp::audio()),
                    now,
                )
                .unwrap();
            source_broker
                .lock()
                .unwrap()
                .propose_target(&app.registration, id, &source, target.clone(), now)
                .unwrap();
            source_adapter.offer_to(id, &target, 20_000).unwrap();
            let offer = source_adapter.drain_egress().pop().unwrap();
            let RealtimeFabricSignal::Offer(projection) = offer.signal else {
                panic!("expected offer");
            };
            let fabric_id = projection.session_id;
            assert!(target_adapter.receive_authenticated(
                &source,
                &target,
                RealtimeFabricSignal::Offer(projection),
                now,
                10_000,
            ));
            target_adapter
                .accept_imported(fabric_id, &source, &target, now)
                .unwrap();

            assert!(!source_adapter.receive_authenticated(
                &attacker,
                &source,
                RealtimeFabricSignal::Cancel(fabric_id),
                now,
                10_000,
            ));
            assert!(
                source_broker
                    .lock()
                    .unwrap()
                    .session(id)
                    .unwrap()
                    .source_live
            );
            assert!(source_adapter.receive_authenticated(
                &target,
                &source,
                RealtimeFabricSignal::Cancel(fabric_id),
                now,
                10_000,
            ));
            assert_eq!(
                source_broker.lock().unwrap().session(id).unwrap().phase,
                RealtimeSessionPhase::SourceLive
            );

            assert!(target_adapter.receive_authenticated(
                &source,
                &target,
                RealtimeFabricSignal::Cancel(fabric_id),
                now,
                10_000,
            ));
            let broker = {
                let state = target_adapter.state.lock().unwrap();
                assert!(state.authorizations.is_empty());
                Arc::clone(&state.broker)
            };
            assert!(
                broker
                    .lock()
                    .unwrap()
                    .session(fabric_id.into_broker_id())
                    .is_none()
            );
        }

        // Phase `adapter_timeout_prunes_process_local_authorization_index`.
        {
            let now = Instant::now();
            let source = device("laptop");
            let target = device("tablet");
            let app = FakeCallingApp::verified("so.tilo.calls", "work");
            let source_broker = Arc::new(Mutex::new(RealtimeSessionBroker::with_limits(
                4,
                Duration::from_secs(60),
                Duration::from_secs(2),
            )));
            let target_broker = Arc::new(Mutex::new(RealtimeSessionBroker::with_limits(
                4,
                Duration::from_secs(60),
                Duration::from_secs(2),
            )));
            let source_adapter =
                BrokerRealtimeFabricAdapter::with_app_verifier(Arc::clone(&source_broker), |_| {
                    true
                });
            let target_adapter =
                BrokerRealtimeFabricAdapter::with_app_verifier(Arc::clone(&target_broker), |_| {
                    true
                });
            let id = source_broker
                .lock()
                .unwrap()
                .register(
                    &app.registration,
                    source.clone(),
                    FakeCallingApp::intent(FakeCallingApp::audio()),
                    now,
                )
                .unwrap();
            source_broker
                .lock()
                .unwrap()
                .propose_target(&app.registration, id, &source, target.clone(), now)
                .unwrap();
            source_adapter.offer_to(id, &target, 20_000).unwrap();
            let offer = source_adapter.drain_egress().pop().unwrap();
            let RealtimeFabricSignal::Offer(projection) = offer.signal else {
                panic!("expected offer");
            };
            let fabric_id = projection.session_id;
            assert!(target_adapter.receive_authenticated(
                &source,
                &target,
                RealtimeFabricSignal::Offer(projection),
                now,
                10_000,
            ));
            target_adapter
                .accept_imported(fabric_id, &source, &target, now)
                .unwrap();
            assert_eq!(target_adapter.state.lock().unwrap().authorizations.len(), 1);

            target_adapter.tick(now + Duration::from_secs(3));
            let broker = {
                let state = target_adapter.state.lock().unwrap();
                assert!(state.authorizations.is_empty());
                Arc::clone(&state.broker)
            };
            assert!(
                broker
                    .lock()
                    .unwrap()
                    .session(fabric_id.into_broker_id())
                    .is_none()
            );
        }
    }
}
