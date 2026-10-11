//! Native realtime continuity host adapter.
//!
//! The shared collaboration broker authenticates the connection and binds
//! app/workspace identity before calling this module. Portal capture leases,
//! device-fabric state, and compositor consent remain native shell authority.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use tokio::sync::mpsc;

use tilo_action_broker::services::{RealtimeRequestContext, RealtimeServices};
struct RealtimeBinding {
    id: crate::shell::realtime_session::RealtimeSessionId,
    app: crate::shell::realtime_session::VerifiedAppRegistration,
    source: crate::shell::personal_continuity::DeviceId,
    owner_peer: u64,
    authorization: Option<crate::shell::realtime_session::CaptureAuthorizationId>,
    target_peer: Option<u64>,
    capture_leases: Vec<crate::portal::capture_broker::CaptureLeaseGrant>,
    imported: bool,
}

struct RealtimeSubscriber {
    app: crate::shell::realtime_session::VerifiedAppRegistration,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

struct RealtimeBridge {
    broker: Arc<Mutex<crate::shell::realtime_session::RealtimeSessionBroker>>,
    fabric: Arc<crate::shell::realtime_session::BrokerRealtimeFabricAdapter>,
    capture: Arc<Mutex<crate::portal::CaptureBroker>>,
    sessions: HashMap<String, RealtimeBinding>,
    subscribers: HashMap<u64, RealtimeSubscriber>,
    pending_imported_offers: VecDeque<crate::shell::realtime_session::ImportedRealtimeFabricOffer>,
    consent_capabilities_issued: std::collections::HashSet<String>,
}

impl Default for RealtimeBridge {
    fn default() -> Self {
        let broker = Arc::new(Mutex::new(
            crate::shell::realtime_session::RealtimeSessionBroker::new(),
        ));
        Self {
            fabric: Arc::new(
                crate::shell::realtime_session::BrokerRealtimeFabricAdapter::new(Arc::clone(
                    &broker,
                )),
            ),
            broker,
            capture: Arc::new(Mutex::new(
                crate::portal::CaptureBroker::with_system_defaults(),
            )),
            sessions: HashMap::new(),
            subscribers: HashMap::new(),
            pending_imported_offers: VecDeque::new(),
            consent_capabilities_issued: std::collections::HashSet::new(),
        }
    }
}

impl RealtimeBridge {
    fn new_token(&self) -> String {
        loop {
            let mut bytes = [0_u8; 16];
            UnwrapErr(SysRng).fill_bytes(&mut bytes);
            let token = hex::encode(bytes);
            if !self.sessions.contains_key(&token) {
                return token;
            }
        }
    }

    fn notify(&mut self, peer: u64, event: &ui::realtime_continuity::RealtimeEvent) -> bool {
        let Ok(bytes) = serde_json::to_vec(event) else {
            return false;
        };
        if bytes.len() > ui::realtime_continuity::MAX_REALTIME_WIRE_BYTES {
            return false;
        }
        let delivered = self
            .subscribers
            .get(&peer)
            .is_some_and(|subscriber| subscriber.tx.send(bytes).is_ok());
        if !delivered {
            self.subscribers.remove(&peer);
        }
        delivered
    }

    fn required_capture_kinds(
        &self,
        id: crate::shell::realtime_session::RealtimeSessionId,
    ) -> Option<Vec<crate::portal::CaptureKind>> {
        let media = self.broker.lock().ok()?.session(id)?.intent.media;
        let mut kinds = Vec::with_capacity(4);
        if media.microphone() {
            kinds.push(crate::portal::CaptureKind::Microphone);
        }
        if media.camera() {
            kinds.push(crate::portal::CaptureKind::Camera);
        }
        if media.screen_share() {
            kinds.push(crate::portal::CaptureKind::Screen);
        }
        if media.share_audio() {
            kinds.push(crate::portal::CaptureKind::SystemAudio);
        }
        Some(kinds)
    }
}

fn broker_realtime_controls(
    controls: ui::realtime_continuity::RealtimeControlIntent,
) -> crate::shell::realtime_session::RealtimeControlState {
    crate::shell::realtime_session::RealtimeControlState {
        microphone_muted: controls.microphone_muted,
        camera_muted: controls.camera_muted,
        screen_share_paused: controls.screen_share_paused,
        share_audio_muted: controls.share_audio_muted,
    }
}

fn app_realtime_controls(
    controls: crate::shell::realtime_session::RealtimeControlState,
) -> ui::realtime_continuity::RealtimeControlIntent {
    ui::realtime_continuity::RealtimeControlIntent {
        microphone_muted: controls.microphone_muted,
        camera_muted: controls.camera_muted,
        screen_share_paused: controls.screen_share_paused,
        share_audio_muted: controls.share_audio_muted,
    }
}

fn acquire_realtime_capture_leases(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    app: &crate::shell::realtime_session::VerifiedAppRegistration,
    session: &str,
    target: &crate::shell::personal_continuity::DeviceId,
    opaque_authorizations: &[String],
    required: &[crate::portal::CaptureKind],
    now: std::time::Instant,
) -> Result<
    Vec<crate::portal::capture_broker::CaptureLeaseGrant>,
    crate::portal::capture_broker::CaptureBrokerError,
> {
    let binding =
        crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())?;
    let authorizations = opaque_authorizations
        .iter()
        .map(|authorization| crate::portal::CaptureAuthorizationId::from_opaque(authorization))
        .collect::<Result<Vec<_>, _>>()?;
    capture
        .lock()
        .map_err(|_| crate::portal::capture_broker::CaptureBrokerError::Unavailable)?
        .acquire_required(&authorizations, &binding, required, now)
}

fn realtime_capture_leases_ready(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    app: &crate::shell::realtime_session::VerifiedAppRegistration,
    session: &str,
    target: &crate::shell::personal_continuity::DeviceId,
    leases: &[crate::portal::capture_broker::CaptureLeaseGrant],
    now: std::time::Instant,
) -> Result<(), crate::portal::capture_broker::CaptureBrokerError> {
    let binding =
        crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())?;
    let mut capture = capture
        .lock()
        .map_err(|_| crate::portal::capture_broker::CaptureBrokerError::Unavailable)?;
    capture.reap_expired(now);
    for grant in leases {
        capture.resolve(grant.lease_id, &binding, now)?;
    }
    drop(capture);
    Ok(())
}

fn release_realtime_capture_session(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    app: &crate::shell::realtime_session::VerifiedAppRegistration,
    session: &str,
    target: &crate::shell::personal_continuity::DeviceId,
) {
    if let Ok(binding) =
        crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())
        && let Ok(mut capture) = capture.lock()
    {
        capture.session_ended(&binding);
    }
}

fn release_realtime_capture_leases(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    leases: &[crate::portal::capture_broker::CaptureLeaseGrant],
) {
    if let Ok(mut capture) = capture.lock() {
        for grant in leases {
            capture.release(grant.lease_id);
        }
    }
}

fn revoke_realtime_capture_authorizations(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    opaque_authorizations: &[String],
) {
    let ids = opaque_authorizations
        .iter()
        .filter_map(|authorization| {
            crate::portal::CaptureAuthorizationId::from_opaque(authorization).ok()
        })
        .collect::<Vec<_>>();
    if let Ok(mut capture) = capture.lock() {
        capture.revoke_authorizations(&ids);
    }
}

fn teardown_imported_realtime_consent(realtime: &mut RealtimeBridge, session: &str) {
    let Some((id, app, source, peer, leases)) = realtime
        .sessions
        .get(session)
        .filter(|binding| binding.imported)
        .map(|binding| {
            (
                binding.id,
                binding.app.clone(),
                binding.source.clone(),
                binding.owner_peer,
                binding.capture_leases.clone(),
            )
        })
    else {
        realtime.consent_capabilities_issued.remove(session);
        return;
    };
    let target = realtime
        .broker
        .lock()
        .ok()
        .and_then(|broker| broker.session(id))
        .and_then(|view| view.target);
    if let Some(target) = target {
        release_realtime_capture_session(&realtime.capture, &app, session, &target);
        let now = std::time::Instant::now();
        let rolled_back = realtime
            .fabric
            .fail_imported(id.fabric_id(), &source, &target, now);
        if rolled_back.is_ok() {
            // `fail_imported` authenticates the exact imported source/target
            // binding and authors Cancel, but deliberately rolls the broker
            // shadow back to SourceLive. This target has no authority to keep
            // that remote-source shadow alive after local policy revocation,
            // so end only the same verified app/session/source binding.
            if let Ok(mut broker) = realtime.broker.lock() {
                let _ = broker.apply_media_action(
                    &app,
                    id,
                    &source,
                    crate::shell::realtime_session::RealtimeMediaAction::Leave,
                    now,
                );
            }
        }
    }
    release_realtime_capture_leases(&realtime.capture, &leases);
    realtime.consent_capabilities_issued.remove(session);
    let _ = realtime.notify(
        peer,
        &ui::realtime_continuity::RealtimeEvent::Ended {
            session: session.to_string(),
        },
    );
    realtime.sessions.remove(session);
}

#[cfg(test)]
struct RealtimeConsentMintHook {
    minted: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
fn realtime_consent_mint_hook() -> &'static Mutex<Option<RealtimeConsentMintHook>> {
    static HOOK: std::sync::OnceLock<Mutex<Option<RealtimeConsentMintHook>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn pause_realtime_consent_after_mint() {
    let hook = realtime_consent_mint_hook()
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(hook) = hook {
        let _ = hook.minted.send(());
        let _ = hook.resume.recv_timeout(tilo_test_env::hang_bound());
    }
}

struct NativeRealtimeState {
    realtime: Mutex<RealtimeBridge>,
    local: crate::shell::personal_continuity::DeviceId,
}

impl NativeRealtimeState {
    fn new(local: crate::shell::personal_continuity::DeviceId) -> Arc<Self> {
        Arc::new(Self {
            realtime: Mutex::new(RealtimeBridge::default()),
            local,
        })
    }
}
/// Cloneable production seam between the certified device fabric and the
/// app-facing realtime broker. The compositor owns one clone for pumping;
/// broker state owns the concrete adapter and all application routing.
#[derive(Clone)]
pub struct RealtimeFabricAppBridge {
    state: Arc<NativeRealtimeState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeSourceUiView {
    pub session: String,
    pub room_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeConsentUiView {
    pub session: String,
    pub room_label: String,
    pub source: crate::shell::personal_continuity::DeviceId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RealtimeContinuityUiView {
    pub sources: Vec<RealtimeSourceUiView>,
    pub consents: Vec<RealtimeConsentUiView>,
}

impl RealtimeFabricAppBridge {
    fn from_state(state: Arc<NativeRealtimeState>) -> Self {
        Self { state }
    }

    #[must_use]
    pub fn adapter(&self) -> Option<crate::shell::realtime_session::SharedRealtimeFabricAdapter> {
        self.state.realtime.lock().ok().map(|realtime| {
            let adapter: crate::shell::realtime_session::SharedRealtimeFabricAdapter =
                realtime.fabric.clone();
            adapter
        })
    }

    /// Shared target-local capture authority for compositor-owned continuity
    /// runtimes. The broker remains shell-private; applications receive only
    /// the semantic consent/session projection.
    pub(crate) fn realtime_capture_broker(
        &self,
    ) -> Option<Arc<Mutex<crate::portal::CaptureBroker>>> {
        self.state.realtime_capture_broker()
    }

    /// Project newly authenticated fabric edges into exact certified local
    /// application subscriptions. Called from the compositor pump after the
    /// device-fabric runtime has drained inbound transport actions.
    pub fn pump_app_events(&self) {
        self.state.pump_realtime_fabric_app_events();
    }

    /// Bounded shell-only projection for compositor chrome. Certified device
    /// identities never enter the application contract.
    #[must_use]
    pub fn ui_view(&self) -> RealtimeContinuityUiView {
        use crate::shell::realtime_session::RealtimeSessionPhase;

        let Ok(realtime) = self.state.realtime.lock() else {
            return RealtimeContinuityUiView::default();
        };
        let Ok(broker) = realtime.broker.lock() else {
            return RealtimeContinuityUiView::default();
        };
        let mut view = RealtimeContinuityUiView::default();
        for (token, binding) in &realtime.sessions {
            let Some(session) = broker.session(binding.id) else {
                continue;
            };
            if !binding.imported && session.phase == RealtimeSessionPhase::SourceLive {
                view.sources.push(RealtimeSourceUiView {
                    session: token.clone(),
                    room_label: session.intent.room.room_id().to_string(),
                });
            } else if binding.imported
                && session.phase == RealtimeSessionPhase::AwaitingTargetAccept
                && !realtime.consent_capabilities_issued.contains(token)
            {
                view.consents.push(RealtimeConsentUiView {
                    session: token.clone(),
                    room_label: session.intent.room.room_id().to_string(),
                    source: session.source,
                });
            }
        }
        view.sources.sort_by(|left, right| {
            left.room_label
                .cmp(&right.room_label)
                .then_with(|| left.session.cmp(&right.session))
        });
        view.consents.sort_by(|left, right| {
            left.room_label
                .cmp(&right.room_label)
                .then_with(|| left.session.cmp(&right.session))
        });
        view
    }

    /// Start an exact source-to-target handoff selected in trusted shell UI.
    ///
    /// # Errors
    ///
    /// Rejects disabled continuity, stale/non-source sessions, identity
    /// mismatches, offline targets, and invalid broker transitions.
    pub fn propose_to(
        &self,
        session: &str,
        target: &crate::shell::personal_continuity::DeviceId,
    ) -> Result<(), String> {
        if !crate::shell::continuity_policy::ContinuityPolicySnapshot::load()
            .permits(crate::shell::continuity_policy::ContinuityCapability::RealtimeSessions)
        {
            return Err("realtime session continuity is disabled".into());
        }
        let realtime = self
            .state
            .realtime
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?;
        let binding = realtime
            .sessions
            .get(session)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        if binding.imported {
            return Err("only a live source session can choose a destination".into());
        }
        let (id, app, source) = (binding.id, binding.app.clone(), binding.source.clone());
        realtime
            .broker
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?
            .propose_target(&app, id, &source, target.clone(), std::time::Instant::now())
            .map_err(|error| error.to_string())?;
        if let Err(error) =
            realtime
                .fabric
                .offer_to(id, target, tilo_clock::unix_now_ms().saturating_add(30_000))
        {
            let _ = realtime.broker.lock().map(|mut broker| {
                let _ = broker.target_failed(&app, id, &source, target, std::time::Instant::now());
            });
            return Err(error.to_string());
        }
        drop(realtime);
        Ok(())
    }

    /// Resolve one imported offer from compositor-owned consent UI. Approval
    /// chooses one currently available local source for every requested
    /// facet and sends only one-shot opaque capabilities to the verified app.
    ///
    /// # Errors
    ///
    /// Rejects stale offers, disabled policy, unavailable capture sources, or
    /// any mismatch between the imported session and this local target.
    pub fn resolve_consent(&self, session: &str, approved: bool) -> Result<(), String> {
        self.resolve_consent_with_policy(session, approved, || {
            crate::shell::continuity_policy::ContinuityPolicySnapshot::load()
                .permits(crate::shell::continuity_policy::ContinuityCapability::RealtimeSessions)
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one consent resolution: policy fence, capability mint, broker transition and rollback"
    )]
    fn resolve_consent_with_policy(
        &self,
        session: &str,
        approved: bool,
        policy_enabled: impl Fn() -> bool,
    ) -> Result<(), String> {
        if !policy_enabled() {
            if let Ok(mut realtime) = self.state.realtime.lock() {
                teardown_imported_realtime_consent(&mut realtime, session);
            }
            return Err("realtime session continuity is disabled".into());
        }
        let (id, app, source, target, peer, required, capture) = {
            let mut realtime = self
                .state
                .realtime
                .lock()
                .map_err(|_| "realtime broker is unavailable".to_string())?;
            let binding = realtime
                .sessions
                .get(session)
                .ok_or_else(|| "unknown realtime session".to_string())?;
            if !binding.imported {
                return Err("only an imported offer can be authorized here".into());
            }
            if realtime.consent_capabilities_issued.contains(session) {
                return Err("capture consent is already being applied".into());
            }
            let target = realtime
                .broker
                .lock()
                .map_err(|_| "realtime broker is unavailable".to_string())?
                .session(binding.id)
                .and_then(|view| view.target)
                .ok_or_else(|| "realtime target binding is unavailable".to_string())?;
            let required = realtime
                .required_capture_kinds(binding.id)
                .ok_or_else(|| "unknown realtime session".to_string())?;
            let result = (
                binding.id,
                binding.app.clone(),
                binding.source.clone(),
                target,
                binding.owner_peer,
                required,
                Arc::clone(&realtime.capture),
            );
            if !realtime
                .consent_capabilities_issued
                .insert(session.to_string())
            {
                return Err("capture consent is already being applied".into());
            }
            drop(realtime);
            result
        };
        if !approved {
            let realtime = self
                .state
                .realtime
                .lock()
                .map_err(|_| "realtime broker is unavailable".to_string())?;
            let declined = realtime
                .fabric
                .fail_imported(id.fabric_id(), &source, &target, std::time::Instant::now())
                .map_err(|error| error.to_string());
            drop(realtime);
            if let Err(error) = declined {
                if let Ok(mut realtime) = self.state.realtime.lock() {
                    realtime.consent_capabilities_issued.remove(session);
                }
                return Err(error);
            }
            if let Ok(mut realtime) = self.state.realtime.lock() {
                realtime.consent_capabilities_issued.remove(session);
                let _ = realtime.notify(
                    peer,
                    &ui::realtime_continuity::RealtimeEvent::Ended {
                        session: session.to_string(),
                    },
                );
                realtime.sessions.remove(session);
            }
            return Ok(());
        }
        let selections = (|| {
            let capture = capture
                .lock()
                .map_err(|_| "target capture broker is unavailable".to_string())?;
            required
                .iter()
                .map(|kind| {
                    capture
                        .enumerate(*kind)
                        .into_iter()
                        .next()
                        .map(|source| (*kind, source.id))
                        .ok_or_else(|| format!("no local {kind:?} source is available"))
                })
                .collect::<Result<Vec<_>, _>>()
        })();
        let selections = match selections {
            Ok(selections) => selections,
            Err(error) => {
                if let Ok(mut realtime) = self.state.realtime.lock() {
                    realtime.consent_capabilities_issued.remove(session);
                }
                return Err(error);
            }
        };
        let authorizations = self.state.authorize_realtime_capture_batch(
            &app,
            session,
            &target,
            &selections,
            std::time::Instant::now(),
        );
        let authorizations = match authorizations {
            Ok(authorizations) => authorizations,
            Err(error) => {
                if let Ok(mut realtime) = self.state.realtime.lock() {
                    realtime.consent_capabilities_issued.remove(session);
                }
                return Err(error);
            }
        };
        #[cfg(test)]
        pause_realtime_consent_after_mint();
        let Ok(mut realtime) = self.state.realtime.lock() else {
            revoke_realtime_capture_authorizations(&capture, &authorizations);
            return Err("realtime broker is unavailable".into());
        };
        let live_binding = realtime.sessions.get(session).is_some_and(|binding| {
            binding.imported
                && binding.id == id
                && binding.app == app
                && binding.source == source
                && binding.owner_peer == peer
        });
        let live_session = realtime
            .broker
            .lock()
            .ok()
            .and_then(|broker| broker.session(id))
            .is_some_and(|view| {
                view.source == source
                    && view.target.as_ref() == Some(&target)
                    && view.phase
                        == crate::shell::realtime_session::RealtimeSessionPhase::AwaitingTargetAccept
            });
        let current_required = realtime.required_capture_kinds(id);
        let still_reserved = realtime.consent_capabilities_issued.contains(session);
        let policy_enabled = policy_enabled();
        if !policy_enabled {
            teardown_imported_realtime_consent(&mut realtime, session);
            drop(realtime);
            revoke_realtime_capture_authorizations(&capture, &authorizations);
            return Err("realtime session continuity was disabled during consent".into());
        }
        if !live_binding
            || !live_session
            || current_required.as_ref() != Some(&required)
            || !still_reserved
        {
            realtime.consent_capabilities_issued.remove(session);
            drop(realtime);
            revoke_realtime_capture_authorizations(&capture, &authorizations);
            return Err("realtime consent expired while capture was being prepared".into());
        }
        let delivered = realtime.notify(
            peer,
            &ui::realtime_continuity::RealtimeEvent::CaptureAuthorized {
                session: session.to_string(),
                capture_authorizations: authorizations.clone(),
            },
        );
        if !delivered {
            realtime.consent_capabilities_issued.remove(session);
            let _ = realtime.fabric.fail_imported(
                id.fabric_id(),
                &source,
                &target,
                std::time::Instant::now(),
            );
            realtime.sessions.remove(session);
            drop(realtime);
            revoke_realtime_capture_authorizations(&capture, &authorizations);
            return Err("realtime target application disconnected during consent".into());
        }
        Ok(())
    }
}

impl NativeRealtimeState {
    #[allow(
        clippy::too_many_lines,
        reason = "offer admission, exact app routing, and lifecycle projection share one bounded broker lock so tokens cannot race subscriber delivery"
    )]
    fn pump_realtime_fabric_app_events(&self) {
        let fabric = match self.realtime.lock() {
            Ok(realtime) => Arc::clone(&realtime.fabric),
            Err(_) => return,
        };
        let offers = fabric.drain_imported_offers();
        let cutovers = fabric.drain_cutovers();
        let lifecycle_events = fabric.drain_lifecycle_events();
        let Ok(mut realtime) = self.realtime.lock() else {
            return;
        };
        for imported in offers {
            if realtime.pending_imported_offers.len()
                == crate::shell::realtime_session::MAX_REALTIME_SESSIONS
            {
                realtime.pending_imported_offers.pop_front();
            }
            realtime.pending_imported_offers.push_back(imported);
        }
        let pending = realtime.pending_imported_offers.len();
        for _ in 0..pending {
            let Some(imported) = realtime.pending_imported_offers.pop_front() else {
                break;
            };
            let Some(peer) = realtime
                .subscribers
                .iter()
                .filter(|(_, subscriber)| {
                    subscriber.app.app_id() == imported.offer.app_id
                        && subscriber.app.workspace_id() == imported.offer.workspace_id
                })
                .map(|(peer, _)| *peer)
                .min()
            else {
                realtime.pending_imported_offers.push_back(imported);
                continue;
            };
            let id = imported.offer.session_id.into_broker_id();
            let live = realtime
                .broker
                .lock()
                .ok()
                .and_then(|broker| broker.session(id))
                .is_some_and(|view| {
                    view.app_id == imported.offer.app_id
                        && view.workspace_id == imported.offer.workspace_id
                        && view.source == imported.source
                        && view.target.as_ref() == Some(&imported.target)
                });
            if !live {
                continue;
            }
            let Ok(app) = crate::shell::realtime_session::VerifiedAppRegistration::new_verified(
                imported.offer.app_id.clone(),
                imported.offer.workspace_id.clone(),
            ) else {
                continue;
            };
            let Ok(intent) = imported.offer.intent() else {
                continue;
            };
            let token = realtime.new_token();
            realtime.sessions.insert(
                token.clone(),
                RealtimeBinding {
                    id,
                    app,
                    source: imported.source.clone(),
                    owner_peer: peer,
                    authorization: None,
                    target_peer: None,
                    capture_leases: Vec::new(),
                    imported: true,
                },
            );
            let event = ui::realtime_continuity::RealtimeEvent::Offer {
                session: token.clone(),
                intent: ui::realtime_continuity::RealtimeResumeIntent {
                    provider: intent.room.provider().to_string(),
                    room_id: intent.room.room_id().to_string(),
                    semantic_position: intent.semantic_position,
                    media: ui::realtime_continuity::RealtimeMediaFacets {
                        microphone: intent.media.microphone(),
                        camera: intent.media.camera(),
                        screen_share: intent.media.screen_share(),
                        share_audio: intent.media.share_audio(),
                    },
                    controls: app_realtime_controls(intent.controls),
                },
            };
            if !realtime.notify(peer, &event) {
                realtime.sessions.remove(&token);
                if realtime.pending_imported_offers.len()
                    == crate::shell::realtime_session::MAX_REALTIME_SESSIONS
                {
                    realtime.pending_imported_offers.pop_front();
                }
                realtime.pending_imported_offers.push_back(imported);
            }
        }
        for directive in cutovers {
            if let Some((token, peer)) = realtime.sessions.iter().find_map(|(token, binding)| {
                (!binding.imported && binding.id == directive.session_id)
                    .then_some((token.clone(), binding.owner_peer))
            }) {
                realtime.notify(
                    peer,
                    &ui::realtime_continuity::RealtimeEvent::StopSource { session: token },
                );
            }
        }
        for event in lifecycle_events {
            let bindings = realtime
                .sessions
                .iter()
                .filter_map(|(token, binding)| {
                    (binding.id == event.session_id).then_some((
                        token.clone(),
                        binding.owner_peer,
                        binding.capture_leases.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            for (token, peer, leases) in bindings {
                let terminal = !matches!(
                    &event.kind,
                    crate::shell::realtime_session::BrokerEventKind::RolledBackToSource { .. }
                );
                let app_event = if terminal {
                    ui::realtime_continuity::RealtimeEvent::Ended {
                        session: token.clone(),
                    }
                } else {
                    ui::realtime_continuity::RealtimeEvent::RolledBack {
                        session: token.clone(),
                    }
                };
                realtime.notify(peer, &app_event);
                release_realtime_capture_leases(&realtime.capture, &leases);
                if terminal {
                    realtime.sessions.remove(&token);
                    realtime.consent_capabilities_issued.remove(&token);
                } else if let Some(binding) = realtime.sessions.get_mut(&token) {
                    binding.authorization = None;
                    binding.target_peer = None;
                    binding.capture_leases.clear();
                }
            }
        }
    }
}

impl NativeRealtimeState {
    /// Shared target-local capture broker used by the portal consent path and
    /// realtime IPC. Local handles and lease IDs never leave the shell.
    pub(crate) fn realtime_capture_broker(
        &self,
    ) -> Option<Arc<Mutex<crate::portal::CaptureBroker>>> {
        self.realtime
            .lock()
            .ok()
            .map(|realtime| Arc::clone(&realtime.capture))
    }

    fn authorize_realtime_capture_batch(
        &self,
        app: &crate::shell::realtime_session::VerifiedAppRegistration,
        session: &str,
        target: &crate::shell::personal_continuity::DeviceId,
        selections: &[(crate::portal::CaptureKind, String)],
        now: std::time::Instant,
    ) -> Result<Vec<String>, String> {
        let realtime = self
            .realtime
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?;
        let binding = realtime
            .sessions
            .get(session)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        let view = realtime
            .broker
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?
            .session(binding.id)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        let required = realtime
            .required_capture_kinds(binding.id)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        let selected_kinds = selections.iter().map(|(kind, _)| *kind).collect::<Vec<_>>();
        if &binding.app != app
            || view.target.as_ref() != Some(target)
            || selected_kinds.len() != required.len()
            || required.iter().any(|kind| !selected_kinds.contains(kind))
        {
            return Err("realtime capture binding mismatch".into());
        }
        let capture_binding =
            crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())
                .map_err(|error| error.to_string())?;
        let capture = Arc::clone(&realtime.capture);
        drop(realtime);
        let selections = selections
            .iter()
            .map(|(kind, source)| (source.clone(), *kind))
            .collect::<Vec<_>>();
        capture
            .lock()
            .map_err(|_| "target capture broker is unavailable".to_string())?
            .authorize_batch(&capture_binding, &selections, now)
            .map(|ids| {
                ids.into_iter()
                    .map(crate::portal::CaptureAuthorizationId::to_opaque)
                    .collect()
            })
            .map_err(|error| error.to_string())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one peer teardown across sessions, captures, offers and fabric routes"
    )]
    fn disconnect(&self, peer_id: u64) {
        let Ok(mut realtime) = self.realtime.lock() else {
            return;
        };
        realtime.subscribers.remove(&peer_id);
        let target_sessions = realtime
            .sessions
            .iter()
            .filter_map(|(token, binding)| {
                (binding.target_peer == Some(peer_id)).then(|| {
                    realtime
                        .broker
                        .lock()
                        .ok()?
                        .session(binding.id)
                        .and_then(|view| {
                            view.target.map(|target| {
                                (
                                    token.clone(),
                                    binding.id,
                                    binding.app.clone(),
                                    binding.source.clone(),
                                    binding.owner_peer,
                                    target,
                                    view.source_live,
                                    binding.imported,
                                )
                            })
                        })
                })?
            })
            .collect::<Vec<_>>();
        for (token, id, app, source, owner_peer, target, source_live, imported) in target_sessions {
            release_realtime_capture_session(&realtime.capture, &app, &token, &target);
            if source_live {
                let failed = if imported {
                    realtime.fabric.fail_imported(
                        id.fabric_id(),
                        &source,
                        &target,
                        std::time::Instant::now(),
                    )
                } else {
                    realtime
                        .broker
                        .lock()
                        .map_err(|_| {
                            crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                        })
                        .and_then(|mut broker| {
                            broker
                                .target_failed(
                                    &app,
                                    id,
                                    &source,
                                    &target,
                                    std::time::Instant::now(),
                                )
                                .map(|_| ())
                        })
                };
                if failed.is_ok() {
                    realtime.notify(
                        owner_peer,
                        &ui::realtime_continuity::RealtimeEvent::RolledBack {
                            session: token.clone(),
                        },
                    );
                }
                if let Some(binding) = realtime.sessions.get_mut(&token) {
                    binding.authorization = None;
                    binding.target_peer = None;
                    binding.capture_leases.clear();
                }
            } else {
                if let Ok(mut broker) = realtime.broker.lock() {
                    let _ = broker.apply_media_action(
                        &app,
                        id,
                        &target,
                        crate::shell::realtime_session::RealtimeMediaAction::Leave,
                        std::time::Instant::now(),
                    );
                }
                realtime.notify(
                    owner_peer,
                    &ui::realtime_continuity::RealtimeEvent::Ended {
                        session: token.clone(),
                    },
                );
                realtime.sessions.remove(&token);
            }
        }
        let owned = realtime
            .sessions
            .iter()
            .filter_map(|(token, binding)| {
                (binding.owner_peer == peer_id).then_some((
                    token.clone(),
                    binding.id,
                    binding.app.clone(),
                    binding.source.clone(),
                    binding.imported,
                ))
            })
            .collect::<Vec<_>>();
        for (token, id, app, source, imported) in owned {
            let target = realtime
                .broker
                .lock()
                .ok()
                .and_then(|broker| broker.session(id))
                .and_then(|view| view.target);
            if let Some(target) = target.as_ref() {
                release_realtime_capture_session(&realtime.capture, &app, &token, target);
            }
            if imported {
                if let Some(target) = target.as_ref() {
                    let _ = realtime.fabric.fail_imported(
                        id.fabric_id(),
                        &source,
                        target,
                        std::time::Instant::now(),
                    );
                }
            } else if let Ok(mut broker) = realtime.broker.lock() {
                let _ = broker.apply_media_action(
                    &app,
                    id,
                    &source,
                    crate::shell::realtime_session::RealtimeMediaAction::Leave,
                    std::time::Instant::now(),
                );
            }
            realtime.sessions.remove(&token);
        }
    }
}

/// Native implementation installed only when a certified device-fabric
/// identity is available. Browser and headless hosts leave the shared service
/// absent, and the RPC fails explicitly as unsupported.
pub(crate) struct NativeRealtimeServices {
    state: Arc<NativeRealtimeState>,
}

impl NativeRealtimeServices {
    pub(crate) fn from_network(
        network: &Arc<crate::shell::network::ShellNetwork>,
    ) -> Option<(Arc<dyn RealtimeServices>, RealtimeFabricAppBridge)> {
        let local = network
            .local_device_fabric_identity()
            .map(|(device, _)| device)?;
        let state = NativeRealtimeState::new(local);
        let bridge = RealtimeFabricAppBridge::from_state(Arc::clone(&state));
        let service: Arc<dyn RealtimeServices> = Arc::new(Self { state });
        Some((service, bridge))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one closed dispatcher over every realtime continuity command"
    )]
    fn command_inner(
        &self,
        peer_id: u64,
        app_id: &str,
        workspace_id: &str,
        bytes: &[u8],
    ) -> Result<ui::realtime_continuity::RealtimeReply, capnp::Error> {
        let app = crate::shell::realtime_session::VerifiedAppRegistration::new_verified(
            app_id,
            workspace_id,
        )
        .map_err(|error| capnp::Error::failed(error.to_string()))?;
        let local = self.state.local.clone();
        let command: ui::realtime_continuity::RealtimeCommand = match serde_json::from_slice(bytes)
        {
            Ok(command) => command,
            Err(error) => return Err(capnp::Error::failed(error.to_string())),
        };
        if !command.valid() {
            return Err(capnp::Error::failed(
                "realtime command contains an unsafe semantic locator".into(),
            ));
        }
        let Ok(mut bridge) = self.state.realtime.lock() else {
            return Err(capnp::Error::failed(
                "realtime broker is unavailable".into(),
            ));
        };
        let now = std::time::Instant::now();
        let reply = match command {
            ui::realtime_continuity::RealtimeCommand::Register { intent } => {
                let mut facets = Vec::with_capacity(4);
                if intent.media.microphone {
                    facets.push(crate::shell::realtime_session::RealtimeMediaFacet::Microphone);
                }
                if intent.media.camera {
                    facets.push(crate::shell::realtime_session::RealtimeMediaFacet::Camera);
                }
                if intent.media.screen_share {
                    facets.push(crate::shell::realtime_session::RealtimeMediaFacet::ScreenShare);
                }
                if intent.media.share_audio {
                    facets.push(crate::shell::realtime_session::RealtimeMediaFacet::ShareAudio);
                }
                let registered = crate::shell::realtime_session::RealtimeResumeIntent {
                    room: match crate::shell::realtime_session::SemanticRoomLocator::new(
                        intent.provider,
                        intent.room_id,
                    ) {
                        Ok(room) => room,
                        Err(error) => return Err(capnp::Error::failed(error.to_string())),
                    },
                    semantic_position: intent.semantic_position,
                    media: match crate::shell::realtime_session::RealtimeMediaSet::new(facets) {
                        Ok(media) => media,
                        Err(error) => return Err(capnp::Error::failed(error.to_string())),
                    },
                    controls: broker_realtime_controls(intent.controls),
                };
                let id = match bridge
                    .broker
                    .lock()
                    .map_err(|_| {
                        crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                    })
                    .and_then(|mut broker| broker.register(&app, local.clone(), registered, now))
                {
                    Ok(id) => id,
                    Err(error) => return Err(capnp::Error::failed(error.to_string())),
                };
                let token = bridge.new_token();
                bridge.sessions.insert(
                    token.clone(),
                    RealtimeBinding {
                        id,
                        app,
                        source: local,
                        owner_peer: peer_id,
                        authorization: None,
                        target_peer: None,
                        capture_leases: Vec::new(),
                        imported: false,
                    },
                );
                ui::realtime_continuity::RealtimeReply::Registered { session: token }
            }
            ui::realtime_continuity::RealtimeCommand::Accept {
                session,
                capture_authorizations,
            } => {
                let Some(binding) = bridge.sessions.get(&session) else {
                    return Err(capnp::Error::failed("unknown realtime session".into()));
                };
                if binding.app != app {
                    return Err(capnp::Error::failed(
                        "realtime session binding mismatch".into(),
                    ));
                }
                if binding.authorization.is_some() || binding.target_peer.is_some() {
                    return Err(capnp::Error::failed(
                        "realtime target already accepted".into(),
                    ));
                }
                let (id, source, imported) = (binding.id, binding.source.clone(), binding.imported);
                let Some(required) = bridge.required_capture_kinds(id) else {
                    return Err(capnp::Error::failed("unknown realtime session".into()));
                };
                let leases = match acquire_realtime_capture_leases(
                    &bridge.capture,
                    &app,
                    &session,
                    &local,
                    &capture_authorizations,
                    &required,
                    now,
                ) {
                    Ok(leases) => leases,
                    Err(error) => return Err(capnp::Error::failed(error.to_string())),
                };
                let authorization =
                    if imported {
                        if let Err(error) =
                            bridge
                                .fabric
                                .accept_imported(id.fabric_id(), &source, &local, now)
                        {
                            release_realtime_capture_leases(&bridge.capture, &leases);
                            return Err(capnp::Error::failed(error.to_string()));
                        }
                        None
                    } else {
                        let authorization = match bridge.broker.lock().map_err(|_| {
                        crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                    }).and_then(|mut broker| {
                        broker.authorize_target_capture(&app, id, &source, &local, now)
                    }) {
                        Ok(authorization) => authorization,
                        Err(error) => {
                            release_realtime_capture_leases(&bridge.capture, &leases);
                            return Err(capnp::Error::failed(error.to_string()));
                        }
                    };
                        if let Err(error) = bridge.broker.lock().map_err(|_| {
                        crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                    }).and_then(|mut broker| {
                        broker.accept_target(&app, id, &source, &local, authorization, now)
                    }) {
                        release_realtime_capture_leases(&bridge.capture, &leases);
                        return Err(capnp::Error::failed(error.to_string()));
                    }
                        Some(authorization)
                    };
                if let Some(binding) = bridge.sessions.get_mut(&session) {
                    binding.authorization = authorization;
                    binding.target_peer = Some(peer_id);
                    binding.capture_leases = leases;
                }
                bridge.consent_capabilities_issued.remove(&session);
                ui::realtime_continuity::RealtimeReply::Accepted
            }
            ui::realtime_continuity::RealtimeCommand::MediaReady { session } => {
                let Some(binding) = bridge.sessions.get(&session) else {
                    return Err(capnp::Error::failed("unknown realtime session".into()));
                };
                if binding.app != app {
                    return Err(capnp::Error::failed(
                        "realtime session binding mismatch".into(),
                    ));
                }
                if !binding.imported && binding.authorization.is_none() {
                    return Err(capnp::Error::failed(
                        "fresh target capture authorization is required".into(),
                    ));
                }
                if binding.target_peer != Some(peer_id) {
                    return Err(capnp::Error::failed(
                        "realtime target binding mismatch".into(),
                    ));
                }
                let (id, owner_peer, source, leases, imported, authorization) = (
                    binding.id,
                    binding.owner_peer,
                    binding.source.clone(),
                    binding.capture_leases.clone(),
                    binding.imported,
                    binding.authorization,
                );
                if let Err(error) = realtime_capture_leases_ready(
                    &bridge.capture,
                    &app,
                    &session,
                    &local,
                    &leases,
                    now,
                ) {
                    if imported {
                        let _ = bridge
                            .fabric
                            .fail_imported(id.fabric_id(), &source, &local, now);
                    } else if let Ok(mut broker) = bridge.broker.lock() {
                        let _ = broker.target_failed(&app, id, &source, &local, now);
                    }
                    if let Some(binding) = bridge.sessions.get_mut(&session) {
                        binding.authorization = None;
                        binding.target_peer = None;
                        binding.capture_leases.clear();
                    }
                    release_realtime_capture_session(&bridge.capture, &app, &session, &local);
                    bridge.notify(
                        owner_peer,
                        &ui::realtime_continuity::RealtimeEvent::RolledBack {
                            session: session.clone(),
                        },
                    );
                    return Err(capnp::Error::failed(error.to_string()));
                }
                let directive = if imported {
                    bridge
                        .fabric
                        .imported_media_ready(id.fabric_id(), &source, &local, now)
                        .map(|()| None)
                } else {
                    let Some(authorization) = authorization else {
                        return Err(capnp::Error::failed(
                            "fresh target capture authorization is required".into(),
                        ));
                    };
                    bridge
                        .broker
                        .lock()
                        .map_err(|_| {
                            crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                        })
                        .and_then(|mut broker| {
                            broker
                                .media_ready(&app, id, &source, &local, authorization, now)
                                .map(Some)
                        })
                };
                let directive = match directive {
                    Ok(directive) => directive,
                    Err(error) => {
                        if imported {
                            let _ =
                                bridge
                                    .fabric
                                    .fail_imported(id.fabric_id(), &source, &local, now);
                        } else if let Ok(mut broker) = bridge.broker.lock() {
                            let _ = broker.target_failed(&app, id, &source, &local, now);
                        }
                        if let Some(binding) = bridge.sessions.get_mut(&session) {
                            binding.authorization = None;
                            binding.target_peer = None;
                            binding.capture_leases.clear();
                        }
                        release_realtime_capture_session(&bridge.capture, &app, &session, &local);
                        bridge.notify(
                            owner_peer,
                            &ui::realtime_continuity::RealtimeEvent::RolledBack {
                                session: session.clone(),
                            },
                        );
                        return Err(capnp::Error::failed(error.to_string()));
                    }
                };
                if directive.is_some_and(|directive| directive.stop_source) {
                    bridge.notify(
                        owner_peer,
                        &ui::realtime_continuity::RealtimeEvent::StopSource {
                            session: session.clone(),
                        },
                    );
                }
                ui::realtime_continuity::RealtimeReply::MediaReady
            }
            ui::realtime_continuity::RealtimeCommand::TargetFailed { session } => {
                let Some(binding) = bridge.sessions.get(&session) else {
                    return Err(capnp::Error::failed("unknown realtime session".into()));
                };
                if binding.app != app {
                    return Err(capnp::Error::failed(
                        "realtime session binding mismatch".into(),
                    ));
                }
                if binding.target_peer != Some(peer_id) {
                    return Err(capnp::Error::failed(
                        "realtime target binding mismatch".into(),
                    ));
                }
                let (id, owner_peer, source) =
                    (binding.id, binding.owner_peer, binding.source.clone());
                let failed = if binding.imported {
                    bridge
                        .fabric
                        .fail_imported(id.fabric_id(), &source, &local, now)
                } else {
                    bridge
                        .broker
                        .lock()
                        .map_err(|_| {
                            crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                        })
                        .and_then(|mut broker| {
                            broker
                                .target_failed(&app, id, &source, &local, now)
                                .map(|_| ())
                        })
                };
                if let Err(error) = failed {
                    return Err(capnp::Error::failed(error.to_string()));
                }
                if let Some(binding) = bridge.sessions.get_mut(&session) {
                    binding.authorization = None;
                    binding.target_peer = None;
                    binding.capture_leases.clear();
                }
                release_realtime_capture_session(&bridge.capture, &app, &session, &local);
                bridge.notify(
                    owner_peer,
                    &ui::realtime_continuity::RealtimeEvent::RolledBack {
                        session: session.clone(),
                    },
                );
                ui::realtime_continuity::RealtimeReply::RolledBack
            }
        };
        drop(bridge);
        Ok(reply)
    }
}

impl RealtimeServices for NativeRealtimeServices {
    fn command(
        &self,
        context: &RealtimeRequestContext,
        command_json: &[u8],
    ) -> Result<Vec<u8>, String> {
        let reply = self
            .command_inner(
                context.peer_id(),
                context.app_id(),
                context.workspace_id(),
                command_json,
            )
            .map_err(|error| error.to_string())?;
        serde_json::to_vec(&reply).map_err(|error| error.to_string())
    }

    fn subscribe(
        &self,
        context: &RealtimeRequestContext,
    ) -> Result<mpsc::UnboundedReceiver<Vec<u8>>, String> {
        let app = crate::shell::realtime_session::VerifiedAppRegistration::new_verified(
            context.app_id(),
            context.workspace_id(),
        )
        .map_err(|error| error.to_string())?;
        let mut realtime = self
            .state
            .realtime
            .lock()
            .map_err(|_| "realtime broker unavailable".to_string())?;
        if realtime.subscribers.contains_key(&context.peer_id()) {
            return Err("this connection already has a realtime subscription".into());
        }
        let (tx, rx) = mpsc::unbounded_channel();
        realtime
            .subscribers
            .insert(context.peer_id(), RealtimeSubscriber { app, tx });
        drop(realtime);
        self.state.pump_realtime_fabric_app_events();
        Ok(rx)
    }

    fn disconnect(&self, peer_id: u64) {
        self.state.disconnect(peer_id);
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;

    #[test]
    fn native_registration_binds_authenticated_context_and_disconnect_revokes_it() {
        let state = NativeRealtimeState::new(
            crate::shell::personal_continuity::DeviceId::new("device-native-7")
                .expect("valid device id"),
        );
        let service = NativeRealtimeServices {
            state: Arc::clone(&state),
        };
        let peer_id = 77;
        let app_id = "so.tilo.video";
        let workspace_id = "workspace-42";
        let command = ui::realtime_continuity::RealtimeCommand::Register {
            intent: ui::realtime_continuity::RealtimeResumeIntent {
                provider: "tilo.video".into(),
                room_id: "call-9".into(),
                semantic_position: Some("participant-3".into()),
                media: ui::realtime_continuity::RealtimeMediaFacets {
                    microphone: true,
                    ..Default::default()
                },
                controls: ui::realtime_continuity::RealtimeControlIntent::default(),
            },
        };
        let reply = service
            .command_inner(
                peer_id,
                app_id,
                workspace_id,
                &serde_json::to_vec(&command).expect("encode command"),
            )
            .expect("native registration succeeds");
        let ui::realtime_continuity::RealtimeReply::Registered { session } = reply else {
            panic!("registration must return a session capability");
        };
        {
            let realtime = state.realtime.lock().expect("realtime state");
            let binding = realtime.sessions.get(&session).expect("registered binding");
            assert_eq!(binding.owner_peer, peer_id);
            assert_eq!(binding.app.app_id(), app_id);
            assert_eq!(binding.app.workspace_id(), workspace_id);
            assert_eq!(binding.source.as_str(), "device-native-7");
        }

        service.disconnect(peer_id);
        assert!(
            state
                .realtime
                .lock()
                .expect("realtime state")
                .sessions
                .is_empty(),
            "disconnect revokes every peer-owned session capability"
        );
    }
}
