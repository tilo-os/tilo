//! Shell-owned runtime for consented certified-phone tethering.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, OnceLock, mpsc};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use zeroize::{Zeroize, Zeroizing};

use crate::shell::device_fabric::{DeviceKind, PersonalFabricAction, PersonalFabricActionEnvelope};
use crate::shell::network::ShellNetwork;
use crate::shell::personal_continuity::DeviceId;
use crate::shell::personal_tethering::{
    PersonalTethering, TetheringEvent, TetheringId, TetheringRoute, TetheringSignal,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TetheringConsentView {
    pub request_id: TetheringId,
    pub requester: DeviceId,
    pub requester_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TetheringDirection {
    Requester,
    Provider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TetheringPhase {
    Requesting,
    ConsentRequired,
    /// Approved here; the hotspot is still starting.
    Preparing,
    /// Granted; this device is still joining the provider's hotspot.
    Connecting,
    Active,
    Denied,
    Expired,
    Offline,
    PolicyDisabled,
    Failed,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TetheringStatusView {
    pub request_id: TetheringId,
    pub peer: DeviceId,
    pub peer_label: String,
    pub direction: TetheringDirection,
    pub phase: TetheringPhase,
    pub expires_unix_ms: u64,
}

impl TetheringStatusView {
    #[must_use]
    pub const fn can_disconnect(&self) -> bool {
        matches!(
            self.phase,
            TetheringPhase::Requesting
                | TetheringPhase::ConsentRequired
                | TetheringPhase::Preparing
                | TetheringPhase::Connecting
                | TetheringPhase::Active
        )
    }
}

#[derive(Clone, Debug)]
enum Command {
    Request {
        request_id: TetheringId,
        provider: DeviceId,
    },
    Decide {
        request_id: TetheringId,
        approved: bool,
    },
    Disconnect {
        request_id: TetheringId,
    },
}

#[derive(Default)]
struct ShellState {
    commands: VecDeque<Command>,
    consents: BTreeMap<TetheringId, TetheringConsentView>,
    statuses: BTreeMap<TetheringId, TetheringStatusView>,
}

fn shell_state() -> &'static Mutex<ShellState> {
    static STATE: OnceLock<Mutex<ShellState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(ShellState::default()))
}

fn set_phase(request_id: TetheringId, phase: TetheringPhase, expires_unix_ms: u64) {
    if let Ok(mut state) = shell_state().lock()
        && let Some(status) = state.statuses.get_mut(&request_id)
    {
        status.phase = phase;
        status.expires_unix_ms = expires_unix_ms;
    }
}

fn set_phase_keeping_expiry(request_id: TetheringId, phase: TetheringPhase) {
    if let Ok(mut state) = shell_state().lock()
        && let Some(status) = state.statuses.get_mut(&request_id)
    {
        status.phase = phase;
    }
}

#[must_use]
pub fn pending_tethering_consents() -> Vec<TetheringConsentView> {
    shell_state().lock().map_or_else(
        |_| Vec::new(),
        |state| state.consents.values().cloned().collect(),
    )
}

#[must_use]
pub fn tethering_statuses() -> Vec<TetheringStatusView> {
    shell_state().lock().map_or_else(
        |_| Vec::new(),
        |state| state.statuses.values().cloned().collect(),
    )
}

/// Queue a request only for an online certified phone. Local-hotspot control
/// remains the existing Quick Settings `NetworkManager` action.
///
/// # Errors
///
/// Returns a message when Wi-Fi Continuity is disabled, `provider` is not an
/// online certified phone, the coordinator is unavailable, or too many
/// requests are active or recorded.
pub fn request_phone_tethering(
    network: &ShellNetwork,
    provider: &DeviceId,
) -> Result<TetheringId, String> {
    if !crate::shell::continuity_policy::ContinuityPolicySnapshot::current()
        .permits(crate::shell::continuity_policy::ContinuityCapability::WifiSettings)
    {
        return Err(crate::shell::i18n::t("tethering-error-continuity-off"));
    }
    let snapshot = network
        .device_fabric()
        .snapshots()
        .get(provider)
        .cloned()
        .filter(|snapshot| snapshot.presence.online && snapshot.presence.kind == DeviceKind::Phone)
        .ok_or_else(|| crate::shell::i18n::t("tethering-error-choose-phone"))?;
    let request_id = crate::random::nonzero::<16>();
    let mut state = shell_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("tethering-error-unavailable"))?;
    if state
        .statuses
        .values()
        .filter(|status| status.can_disconnect())
        .count()
        >= 16
    {
        return Err(crate::shell::i18n::t("tethering-error-too-many"));
    }
    while state.statuses.len() >= 32 {
        let Some(terminal) = state
            .statuses
            .iter()
            .find(|(_, status)| !status.can_disconnect())
            .map(|(id, _)| *id)
        else {
            return Err(crate::shell::i18n::t("tethering-error-history-full"));
        };
        state.statuses.remove(&terminal);
    }
    state.statuses.insert(
        request_id,
        TetheringStatusView {
            request_id,
            peer: provider.clone(),
            peer_label: snapshot.presence.label,
            direction: TetheringDirection::Requester,
            phase: TetheringPhase::Requesting,
            expires_unix_ms: 0,
        },
    );
    state.commands.push_back(Command::Request {
        request_id,
        provider: provider.clone(),
    });
    drop(state);
    Ok(request_id)
}

/// Queue the end of an active tethering session.
///
/// # Errors
///
/// Returns a message when the coordinator is unavailable or the session is
/// no longer active.
pub fn disconnect_phone_tethering(request_id: TetheringId) -> Result<(), String> {
    let mut state = shell_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("tethering-error-unavailable"))?;
    if !state
        .statuses
        .get(&request_id)
        .is_some_and(TetheringStatusView::can_disconnect)
    {
        return Err(crate::shell::i18n::t("tethering-error-session-inactive"));
    }
    state.commands.push_back(Command::Disconnect { request_id });
    drop(state);
    Ok(())
}

/// Queue the local user's answer to a pending tethering request.
///
/// # Errors
///
/// Returns a message when the coordinator is unavailable or the request is
/// no longer pending.
pub fn authorize_phone_tethering(request_id: TetheringId, approved: bool) -> Result<(), String> {
    let mut state = shell_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("tethering-error-unavailable"))?;
    if !state.consents.contains_key(&request_id) {
        return Err(crate::shell::i18n::t("tethering-error-request-gone"));
    }
    state.commands.push_back(Command::Decide {
        request_id,
        approved,
    });
    drop(state);
    Ok(())
}

type WifiCredential = crate::shell::device_fabric::WifiCredential;

/// One `NetworkManager` operation a tethering session needs.
///
/// These run on the runtime's serial worker, never on the compositor thread:
/// `nmcli` may wait 35 s to join a network and about 22 s to bring a hotspot
/// up, and the desktop has to keep painting meanwhile. One worker runs them in
/// submission order, so a stop never overtakes the start it ends and a
/// disconnect never overtakes the join it undoes.
///
/// Deliberately not `Debug`: two variants carry a Wi-Fi passphrase.
pub(crate) enum TetheringJob {
    StartHotspot {
        request_id: TetheringId,
        requester: DeviceId,
        ssid: String,
        password: Zeroizing<String>,
    },
    StopHotspot,
    Connect {
        request_id: TetheringId,
        ssid: String,
        password: Zeroizing<String>,
    },
    Disconnect {
        ssid: String,
    },
}

/// What a finished [`TetheringJob`] reports to the next pump. Stops and
/// disconnects are best effort and report nothing.
///
/// Deliberately not `Debug`: a started hotspot hands its passphrase back for
/// sealing.
pub(crate) enum TetheringJobResult {
    HotspotStarted {
        request_id: TetheringId,
        requester: DeviceId,
        ssid: String,
        password: Zeroizing<String>,
    },
    HotspotFailed {
        request_id: TetheringId,
        requester: DeviceId,
        error: String,
    },
    Connected {
        request_id: TetheringId,
    },
    ConnectFailed {
        request_id: TetheringId,
        error: String,
    },
}

pub(crate) trait TetheringHost {
    fn send(&mut self, target: &DeviceId, signal: TetheringSignal) -> Result<(), String>;
    /// Queue a `NetworkManager` operation. Never blocks.
    fn submit(&mut self, job: TetheringJob);
    /// Operations that finished since the last call. Never blocks.
    fn completions(&mut self) -> Vec<TetheringJobResult>;
    fn open_credential(&mut self, credential: &WifiCredential)
    -> Result<Zeroizing<String>, String>;
    fn seal_credential(
        &mut self,
        requester: &DeviceId,
        ssid: &str,
        password: &str,
    ) -> Result<WifiCredential, String>;
}

/// The serial `NetworkManager` worker, started by the first job.
#[derive(Default)]
struct TetheringWorker {
    channel: Option<(
        mpsc::Sender<TetheringJob>,
        mpsc::Receiver<TetheringJobResult>,
    )>,
}

impl TetheringWorker {
    fn submit(&mut self, job: TetheringJob) {
        let Err(mpsc::SendError(job)) = self.channel.get_or_insert_with(Self::spawn).0.send(job)
        else {
            return;
        };
        // The thread is gone; start another rather than drop the job.
        self.channel = None;
        if self
            .channel
            .get_or_insert_with(Self::spawn)
            .0
            .send(job)
            .is_err()
        {
            tracing::warn!("tethering worker unavailable; NetworkManager operation dropped");
        }
    }

    fn completions(&self) -> Vec<TetheringJobResult> {
        self.channel
            .as_ref()
            .map_or_else(Vec::new, |(_, results)| results.try_iter().collect())
    }

    fn spawn() -> (
        mpsc::Sender<TetheringJob>,
        mpsc::Receiver<TetheringJobResult>,
    ) {
        let (jobs, queued) = mpsc::channel::<TetheringJob>();
        let (finished, results) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("tilo-tethering".into())
            .spawn(move || {
                for job in queued {
                    let result = match job {
                        TetheringJob::StartHotspot {
                            request_id,
                            requester,
                            ssid,
                            password,
                        } => {
                            let mut config = os_actions::HotspotConfiguration {
                                ssid: ssid.clone(),
                                password: password.as_str().to_owned(),
                                band: os_actions::HotspotBand::Automatic,
                            };
                            let configured = os_actions::configure_hotspot(&config);
                            config.password.zeroize();
                            Some(match configured {
                                Ok(()) => TetheringJobResult::HotspotStarted {
                                    request_id,
                                    requester,
                                    ssid,
                                    password,
                                },
                                Err(error) => TetheringJobResult::HotspotFailed {
                                    request_id,
                                    requester,
                                    error,
                                },
                            })
                        }
                        TetheringJob::StopHotspot => {
                            let _ = os_actions::set_hotspot(false);
                            None
                        }
                        TetheringJob::Connect {
                            request_id,
                            ssid,
                            password,
                        } => Some(
                            match os_actions::connect_wifi(&ssid, Some(password.as_str())) {
                                Ok(()) => TetheringJobResult::Connected { request_id },
                                Err(error) => {
                                    TetheringJobResult::ConnectFailed { request_id, error }
                                }
                            },
                        ),
                        TetheringJob::Disconnect { ssid } => {
                            let _ = os_actions::disconnect_wifi(&ssid);
                            None
                        }
                    };
                    if let Some(result) = result
                        && finished.send(result).is_err()
                    {
                        break;
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start the tethering worker");
        }
        (jobs, results)
    }
}

struct ProductionHost<'a> {
    network: &'a ShellNetwork,
    worker: &'a mut TetheringWorker,
}

impl TetheringHost for ProductionHost<'_> {
    fn send(&mut self, target: &DeviceId, signal: TetheringSignal) -> Result<(), String> {
        self.network
            .send_personal_fabric_action(target, PersonalFabricAction::Tethering { signal })
            .map(|_| ())
    }

    fn submit(&mut self, job: TetheringJob) {
        self.worker.submit(job);
    }

    fn completions(&mut self) -> Vec<TetheringJobResult> {
        self.worker.completions()
    }

    fn open_credential(
        &mut self,
        credential: &WifiCredential,
    ) -> Result<Zeroizing<String>, String> {
        self.network
            .open_tethering_credential(credential)
            .map(Zeroizing::new)
    }

    fn seal_credential(
        &mut self,
        requester: &DeviceId,
        ssid: &str,
        password: &str,
    ) -> Result<WifiCredential, String> {
        self.network
            .seal_tethering_credential(requester, ssid, password)
    }
}

/// A requester-side join `NetworkManager` is still performing.
struct PendingConnect {
    provider: DeviceId,
    ssid: String,
    expires_unix_ms: u64,
}

/// The provider's answer to one request, once its hotspot is known.
struct ProviderDecision {
    request_id: TetheringId,
    requester: DeviceId,
    /// The person approved, so a hotspot may be up and must come down on failure.
    approved: bool,
    credential: Option<WifiCredential>,
}

#[derive(Default)]
pub(crate) struct PersonalTetheringRuntime {
    protocol: PersonalTethering,
    provider_leases: BTreeMap<TetheringId, DeviceId>,
    requester_connections: BTreeMap<TetheringId, (DeviceId, String)>,
    online_peers: BTreeSet<DeviceId>,
    /// The approved request whose hotspot `NetworkManager` is still starting.
    pending_hotspot: Option<TetheringId>,
    /// Joins `NetworkManager` is still performing, by request.
    pending_connects: BTreeMap<TetheringId, PendingConnect>,
    worker: TetheringWorker,
}

impl PersonalTetheringRuntime {
    pub(crate) fn pump(&mut self, network: &ShellNetwork, permitted: bool, now_unix_ms: u64) {
        let Some((local, label)) = network.local_device_fabric_identity() else {
            return;
        };
        let mut worker = std::mem::take(&mut self.worker);
        let mut host = ProductionHost {
            network,
            worker: &mut worker,
        };
        self.pump_with(&mut host, &local, &label, permitted, now_unix_ms);
        let online = network.online_device_ids();
        let previous = std::mem::take(&mut self.online_peers);
        for peer in previous.difference(&online) {
            self.peer_unavailable(&mut host, &local, peer);
        }
        self.online_peers = online;
        self.worker = worker;
    }

    pub(crate) fn handle_action(
        &mut self,
        network: &ShellNetwork,
        envelope: &PersonalFabricActionEnvelope,
        now_unix_ms: u64,
    ) -> bool {
        let PersonalFabricAction::Tethering { signal } = &envelope.action else {
            return false;
        };
        let Some((local, _)) = network.local_device_fabric_identity() else {
            return true;
        };
        let mut worker = std::mem::take(&mut self.worker);
        let mut host = ProductionHost {
            network,
            worker: &mut worker,
        };
        self.handle_signal(&mut host, &local, &envelope.source, signal, now_unix_ms);
        self.worker = worker;
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the tethering pump keeps expiry, command and peer steps in one ordered tick"
    )]
    fn pump_with<H: TetheringHost>(
        &mut self,
        host: &mut H,
        local: &DeviceId,
        label: &str,
        permitted: bool,
        now_unix_ms: u64,
    ) {
        if !permitted {
            self.teardown_all(host, local);
            self.apply_completions(host, local, now_unix_ms);
            return;
        }
        let expired_requests = if let Ok(mut state) = shell_state().lock() {
            let expired = state
                .statuses
                .iter()
                .filter(|(_, status)| {
                    status.can_disconnect()
                        && status.expires_unix_ms != 0
                        && status.expires_unix_ms <= now_unix_ms
                })
                .map(|(request_id, _)| *request_id)
                .collect::<Vec<_>>();
            for request_id in expired {
                if let Some(status) = state.statuses.get_mut(&request_id) {
                    status.phase = TetheringPhase::Expired;
                }
                state.consents.remove(&request_id);
            }
            state
                .statuses
                .iter()
                .filter(|(_, status)| status.phase == TetheringPhase::Expired)
                .map(|(request_id, _)| *request_id)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for request_id in expired_requests {
            self.clear_pending_hotspot(request_id);
            self.end_requester_connection(host, request_id);
        }
        let had_provider_lease = !self.provider_leases.is_empty();
        for stop in self.protocol.expire(local, now_unix_ms) {
            self.provider_leases.remove(&signal_id(&stop));
            let target = signal_target(&stop);
            let _ = host.send(&target, stop);
        }
        if had_provider_lease && self.provider_leases.is_empty() {
            host.submit(TetheringJob::StopHotspot);
        }
        let commands = shell_state().lock().map_or_else(
            |_| Vec::new(),
            |mut state| state.commands.drain(..).collect::<Vec<_>>(),
        );
        for command in commands {
            match command {
                Command::Request {
                    request_id,
                    provider,
                } => {
                    if let Ok(signal) = self.protocol.request(
                        TetheringRoute::CertifiedPhone(provider.clone()),
                        local,
                        label,
                        request_id,
                        now_unix_ms,
                    ) {
                        set_phase(
                            request_id,
                            TetheringPhase::Requesting,
                            signal_expiry(&signal),
                        );
                        if host.send(&provider, signal).is_err() {
                            self.protocol.peer_unavailable(local, &provider);
                            set_phase(request_id, TetheringPhase::Failed, 0);
                        }
                    } else {
                        set_phase(request_id, TetheringPhase::Failed, 0);
                    }
                }
                Command::Decide {
                    request_id,
                    approved,
                } => {
                    let consent = shell_state()
                        .lock()
                        .ok()
                        .and_then(|mut state| state.consents.remove(&request_id));
                    let Some(consent) = consent else { continue };
                    if approved && self.provider_leases.is_empty() && self.pending_hotspot.is_none()
                    {
                        // The grant is sent once the hotspot is up, from
                        // `apply_completions`; until then the request stays
                        // pending in the protocol and can still be cancelled.
                        self.pending_hotspot = Some(request_id);
                        set_phase_keeping_expiry(request_id, TetheringPhase::Preparing);
                        host.submit(TetheringJob::StartHotspot {
                            request_id,
                            requester: consent.requester,
                            ssid: hotspot_ssid(label),
                            password: hotspot_password(),
                        });
                    } else {
                        self.finish_decision(
                            host,
                            local,
                            ProviderDecision {
                                request_id,
                                requester: consent.requester,
                                approved: false,
                                credential: None,
                            },
                            now_unix_ms,
                        );
                    }
                }
                Command::Disconnect { request_id } => {
                    if self.provider_leases.remove(&request_id).is_some() {
                        if let Ok(stop) = self.protocol.stop_provider_lease(local, request_id) {
                            let target = signal_target(&stop);
                            let _ = host.send(&target, stop);
                        }
                        host.submit(TetheringJob::StopHotspot);
                    } else if self.pending_hotspot == Some(request_id) {
                        // Decline the request whose hotspot is still starting.
                        // Its completion then finds no request to grant and
                        // stops the hotspot.
                        if let Ok(signals) =
                            self.protocol
                                .decide(local, request_id, false, None, None, now_unix_ms)
                        {
                            for signal in signals {
                                let target = signal_target(&signal);
                                let _ = host.send(&target, signal);
                            }
                        }
                    } else if let Ok(cancel) = self.protocol.cancel_request(local, request_id) {
                        self.end_requester_connection(host, request_id);
                        let target = signal_target(&cancel);
                        let _ = host.send(&target, cancel);
                    }
                    set_phase(request_id, TetheringPhase::Disconnected, 0);
                }
            }
        }
        self.apply_completions(host, local, now_unix_ms);
    }

    /// Send the provider's decision and record its outcome. A failure after
    /// approval takes the hotspot down again, and a grant that could not be
    /// delivered also ends every session with that requester.
    fn finish_decision<H: TetheringHost>(
        &mut self,
        host: &mut H,
        local: &DeviceId,
        decision: ProviderDecision,
        now_unix_ms: u64,
    ) {
        let ProviderDecision {
            request_id,
            requester,
            approved,
            credential,
        } = decision;
        let accepted = credential.is_some();
        let nonce = accepted.then(crate::random::nonzero::<16>);
        let Ok(signals) =
            self.protocol
                .decide(local, request_id, accepted, nonce, credential, now_unix_ms)
        else {
            if approved {
                host.submit(TetheringJob::StopHotspot);
            }
            return;
        };
        let mut sent = true;
        for signal in signals {
            if host.send(&requester, signal).is_err() {
                sent = false;
                break;
            }
        }
        if accepted && sent {
            self.provider_leases.insert(request_id, requester);
            set_phase(
                request_id,
                TetheringPhase::Active,
                now_unix_ms.saturating_add(crate::shell::personal_tethering::GRANT_TTL_MS),
            );
        } else if approved {
            host.submit(TetheringJob::StopHotspot);
            for stop in self.protocol.peer_unavailable(local, &requester) {
                let _ = host.send(&requester, stop);
            }
            set_phase(request_id, TetheringPhase::Failed, 0);
        } else {
            set_phase(request_id, TetheringPhase::Denied, 0);
        }
    }

    /// Take in what the worker finished since the last tick.
    fn apply_completions<H: TetheringHost>(
        &mut self,
        host: &mut H,
        local: &DeviceId,
        now_unix_ms: u64,
    ) {
        for result in host.completions() {
            match result {
                TetheringJobResult::HotspotStarted {
                    request_id,
                    requester,
                    ssid,
                    password,
                } => {
                    self.clear_pending_hotspot(request_id);
                    let credential = match host.seal_credential(&requester, &ssid, &password) {
                        Ok(credential) => Some(credential),
                        Err(error) => {
                            tracing::warn!(%error, "could not seal the tethering credential");
                            None
                        }
                    };
                    drop(password);
                    self.finish_decision(
                        host,
                        local,
                        ProviderDecision {
                            request_id,
                            requester,
                            approved: true,
                            credential,
                        },
                        now_unix_ms,
                    );
                }
                TetheringJobResult::HotspotFailed {
                    request_id,
                    requester,
                    error,
                } => {
                    self.clear_pending_hotspot(request_id);
                    tracing::warn!(%error, "NetworkManager could not start the tethering hotspot");
                    self.finish_decision(
                        host,
                        local,
                        ProviderDecision {
                            request_id,
                            requester,
                            approved: true,
                            credential: None,
                        },
                        now_unix_ms,
                    );
                }
                TetheringJobResult::Connected { request_id } => {
                    // A join whose session ended meanwhile was already queued
                    // a disconnect behind it, so there is nothing to record.
                    if let Some(pending) = self.pending_connects.remove(&request_id) {
                        self.requester_connections
                            .insert(request_id, (pending.provider, pending.ssid));
                        set_phase(request_id, TetheringPhase::Active, pending.expires_unix_ms);
                    }
                }
                TetheringJobResult::ConnectFailed { request_id, error } => {
                    if let Some(pending) = self.pending_connects.remove(&request_id) {
                        tracing::warn!(%error, "NetworkManager could not join the phone's hotspot");
                        if let Ok(cancel) = self.protocol.cancel_request(local, request_id) {
                            let _ = host.send(&pending.provider, cancel);
                        }
                        set_phase(request_id, TetheringPhase::Failed, 0);
                    }
                }
            }
        }
    }

    fn clear_pending_hotspot(&mut self, request_id: TetheringId) {
        if self.pending_hotspot == Some(request_id) {
            self.pending_hotspot = None;
        }
    }

    /// Forget this device's join for `request_id` and queue its disconnect.
    /// A join still in flight is disconnected too: the worker is serial, so
    /// the disconnect runs once that join has finished.
    fn end_requester_connection<H: TetheringHost>(
        &mut self,
        host: &mut H,
        request_id: TetheringId,
    ) -> bool {
        let ssid = self
            .requester_connections
            .remove(&request_id)
            .map(|(_, ssid)| ssid)
            .or_else(|| {
                self.pending_connects
                    .remove(&request_id)
                    .map(|pending| pending.ssid)
            });
        let Some(ssid) = ssid else {
            return false;
        };
        host.submit(TetheringJob::Disconnect { ssid });
        true
    }

    fn handle_signal<H: TetheringHost>(
        &mut self,
        host: &mut H,
        local: &DeviceId,
        source: &DeviceId,
        signal: &TetheringSignal,
        now_unix_ms: u64,
    ) {
        match signal {
            TetheringSignal::Request { .. } => {
                if let Ok(TetheringEvent::ConsentRequired {
                    request_id,
                    requester,
                    requester_label,
                    ..
                }) = self
                    .protocol
                    .receive_request(source, local, signal, now_unix_ms)
                    && let Ok(mut state) = shell_state().lock()
                {
                    let peer_label = requester_label.clone();
                    state.consents.insert(
                        request_id,
                        TetheringConsentView {
                            request_id,
                            requester,
                            requester_label,
                        },
                    );
                    state.statuses.insert(
                        request_id,
                        TetheringStatusView {
                            request_id,
                            peer: source.clone(),
                            peer_label,
                            direction: TetheringDirection::Provider,
                            phase: TetheringPhase::ConsentRequired,
                            expires_unix_ms: signal_expiry(signal),
                        },
                    );
                }
            }
            TetheringSignal::Decision { .. } => {
                if let Ok(event) =
                    self.protocol
                        .receive_decision(source, local, signal, now_unix_ms)
                {
                    match event {
                        Some(TetheringEvent::Rejected { request_id, .. }) => {
                            set_phase(request_id, TetheringPhase::Denied, 0);
                        }
                        None => set_phase(
                            signal_id(signal),
                            TetheringPhase::Requesting,
                            signal_expiry(signal),
                        ),
                        _ => {}
                    }
                }
            }
            TetheringSignal::Credential { .. } => {
                self.receive_credential(host, local, source, signal, now_unix_ms);
            }
            TetheringSignal::Stop { .. } => {
                if let Ok(TetheringEvent::Ended { request_id, .. }) =
                    self.protocol.receive_stop(source, local, signal)
                    && self.end_requester_connection(host, request_id)
                {
                    set_phase(request_id, TetheringPhase::Disconnected, 0);
                }
            }
            TetheringSignal::Cancel { .. } => {
                if let Ok(TetheringEvent::Ended { request_id, .. }) =
                    self.protocol.receive_cancel(source, local, signal)
                {
                    let was_active = self.provider_leases.remove(&request_id).is_some();
                    if was_active {
                        host.submit(TetheringJob::StopHotspot);
                    }
                    if let Ok(mut state) = shell_state().lock() {
                        state.consents.remove(&request_id);
                    }
                    set_phase(request_id, TetheringPhase::Disconnected, 0);
                }
            }
        }
    }

    /// Queue the join a granted credential offers. The passphrase is opened
    /// here, which is cryptography only; `nmcli` runs on the worker.
    fn receive_credential<H: TetheringHost>(
        &mut self,
        host: &mut H,
        local: &DeviceId,
        source: &DeviceId,
        signal: &TetheringSignal,
        now_unix_ms: u64,
    ) {
        let Ok(TetheringEvent::ConnectOffered {
            request_id,
            credential,
            ..
        }) = self
            .protocol
            .receive_credential(source, local, signal, now_unix_ms)
        else {
            return;
        };
        if self.pending_connects.contains_key(&request_id)
            || self.requester_connections.contains_key(&request_id)
        {
            // A repeated credential for a join already under way.
            return;
        }
        match host.open_credential(&credential) {
            Ok(password) => {
                let expires_unix_ms = signal_expiry(signal);
                self.pending_connects.insert(
                    request_id,
                    PendingConnect {
                        provider: source.clone(),
                        ssid: credential.ssid.clone(),
                        expires_unix_ms,
                    },
                );
                set_phase(request_id, TetheringPhase::Connecting, expires_unix_ms);
                host.submit(TetheringJob::Connect {
                    request_id,
                    ssid: credential.ssid,
                    password,
                });
            }
            Err(error) => {
                tracing::warn!(%error, "could not open the tethering credential");
                if let Ok(cancel) = self.protocol.cancel_request(local, request_id) {
                    let _ = host.send(source, cancel);
                }
                set_phase(request_id, TetheringPhase::Failed, 0);
            }
        }
    }

    fn peer_unavailable<H: TetheringHost>(
        &mut self,
        host: &mut H,
        local: &DeviceId,
        peer: &DeviceId,
    ) {
        let had_provider_lease = !self.provider_leases.is_empty();
        for stop in self.protocol.peer_unavailable(local, peer) {
            let _ = host.send(peer, stop);
        }
        self.provider_leases
            .retain(|_, requester| requester != peer);
        let ended = self
            .requester_connections
            .iter()
            .filter(|(_, (provider, _))| provider == peer)
            .map(|(request_id, _)| *request_id)
            .chain(
                self.pending_connects
                    .iter()
                    .filter(|(_, pending)| &pending.provider == peer)
                    .map(|(request_id, _)| *request_id),
            )
            .collect::<Vec<_>>();
        for request_id in ended {
            self.end_requester_connection(host, request_id);
        }
        if had_provider_lease && self.provider_leases.is_empty() {
            host.submit(TetheringJob::StopHotspot);
        }
        if let Ok(mut state) = shell_state().lock() {
            state
                .consents
                .retain(|_, consent| &consent.requester != peer);
            for status in state
                .statuses
                .values_mut()
                .filter(|status| &status.peer == peer && status.can_disconnect())
            {
                status.phase = TetheringPhase::Offline;
                status.expires_unix_ms = 0;
            }
        }
    }

    fn teardown_all<H: TetheringHost>(&mut self, host: &mut H, local: &DeviceId) {
        for stop in self.protocol.policy_disabled(local) {
            let target = signal_target(&stop);
            let _ = host.send(&target, stop);
        }
        if !self.provider_leases.is_empty() {
            host.submit(TetheringJob::StopHotspot);
        }
        let joined = self
            .requester_connections
            .keys()
            .chain(self.pending_connects.keys())
            .copied()
            .collect::<Vec<_>>();
        for request_id in joined {
            self.end_requester_connection(host, request_id);
        }
        self.provider_leases.clear();
        if let Ok(mut state) = shell_state().lock() {
            state.consents.clear();
            state.commands.clear();
            for status in state
                .statuses
                .values_mut()
                .filter(|status| status.can_disconnect())
            {
                status.phase = TetheringPhase::PolicyDisabled;
                status.expires_unix_ms = 0;
            }
        }
    }
}

/// The hotspot's network name: this device's label, printable and short.
fn hotspot_ssid(label: &str) -> String {
    let suffix = label
        .chars()
        .filter(|c| !c.is_control())
        .take(20)
        .collect::<String>();
    format!("Tilo {suffix}")
}

/// A fresh single-use hotspot passphrase.
fn hotspot_password() -> Zeroizing<String> {
    let mut secret = Zeroizing::new([0_u8; 16]);
    UnwrapErr(SysRng).fill_bytes(secret.as_mut_slice());
    Zeroizing::new(hex::encode(secret.as_slice()))
}

fn signal_target(signal: &TetheringSignal) -> DeviceId {
    match signal {
        TetheringSignal::Request { provider, .. } | TetheringSignal::Cancel { provider, .. } => {
            provider.clone()
        }
        TetheringSignal::Decision { requester, .. }
        | TetheringSignal::Credential { requester, .. }
        | TetheringSignal::Stop { requester, .. } => requester.clone(),
    }
}

fn signal_id(signal: &TetheringSignal) -> TetheringId {
    match signal {
        TetheringSignal::Request { request_id, .. }
        | TetheringSignal::Decision { request_id, .. }
        | TetheringSignal::Credential { request_id, .. }
        | TetheringSignal::Stop { request_id, .. }
        | TetheringSignal::Cancel { request_id, .. } => *request_id,
    }
}

fn signal_expiry(signal: &TetheringSignal) -> u64 {
    match signal {
        TetheringSignal::Request {
            expires_unix_ms, ..
        }
        | TetheringSignal::Decision {
            expires_unix_ms, ..
        }
        | TetheringSignal::Credential {
            expires_unix_ms, ..
        } => *expires_unix_ms,
        TetheringSignal::Stop { .. } | TetheringSignal::Cancel { .. } => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn reset_shell_state() {
        *shell_state().lock().unwrap() = ShellState::default();
    }

    struct Host {
        credential: crate::shell::device_fabric::WifiCredential,
        sent: Vec<(DeviceId, TetheringSignal)>,
        hotspot_starts: usize,
        hotspot_stops: usize,
        fail_send_after: Option<usize>,
        fail_hotspot: bool,
        fail_connect: bool,
        seal_error: Option<&'static str>,
        /// Finish each job as it is queued, as a quick `NetworkManager`
        /// would. Otherwise results wait in `held` until a test releases them.
        complete_jobs: bool,
        connects: Vec<TetheringId>,
        disconnected: Vec<String>,
        held: Vec<TetheringJobResult>,
        finished: Vec<TetheringJobResult>,
    }

    impl Host {
        /// `NetworkManager` finishes every job queued so far.
        fn release(&mut self) {
            self.finished.append(&mut self.held);
        }
    }

    impl TetheringHost for Host {
        fn send(&mut self, target: &DeviceId, signal: TetheringSignal) -> Result<(), String> {
            if self.fail_send_after == Some(self.sent.len()) {
                return Err("transport full".into());
            }
            self.sent.push((target.clone(), signal));
            Ok(())
        }

        fn submit(&mut self, job: TetheringJob) {
            let result = match job {
                TetheringJob::StartHotspot {
                    request_id,
                    requester,
                    ssid,
                    password,
                } => {
                    self.hotspot_starts = self.hotspot_starts.saturating_add(1);
                    if self.fail_hotspot {
                        TetheringJobResult::HotspotFailed {
                            request_id,
                            requester,
                            error: "NetworkManager refused hotspot".into(),
                        }
                    } else {
                        TetheringJobResult::HotspotStarted {
                            request_id,
                            requester,
                            ssid,
                            password,
                        }
                    }
                }
                TetheringJob::StopHotspot => {
                    self.hotspot_stops = self.hotspot_stops.saturating_add(1);
                    return;
                }
                TetheringJob::Connect { request_id, .. } => {
                    self.connects.push(request_id);
                    if self.fail_connect {
                        TetheringJobResult::ConnectFailed {
                            request_id,
                            error: "NetworkManager refused connection".into(),
                        }
                    } else {
                        TetheringJobResult::Connected { request_id }
                    }
                }
                TetheringJob::Disconnect { ssid } => {
                    self.disconnected.push(ssid);
                    return;
                }
            };
            if self.complete_jobs {
                self.finished.push(result);
            } else {
                self.held.push(result);
            }
        }

        fn completions(&mut self) -> Vec<TetheringJobResult> {
            std::mem::take(&mut self.finished)
        }

        fn open_credential(
            &mut self,
            _credential: &crate::shell::device_fabric::WifiCredential,
        ) -> Result<Zeroizing<String>, String> {
            Ok(Zeroizing::new("not-a-real-password".into()))
        }

        fn seal_credential(
            &mut self,
            _requester: &DeviceId,
            _ssid: &str,
            _password: &str,
        ) -> Result<crate::shell::device_fabric::WifiCredential, String> {
            if let Some(error) = self.seal_error {
                return Err(error.into());
            }
            Ok(self.credential.clone())
        }
    }

    fn device(seed: u8) -> DeviceId {
        DeviceId::new(hex::encode(
            SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        ))
        .unwrap()
    }

    fn host(requester: &DeviceId, provider: &DeviceId) -> Host {
        let credential = crate::shell::device_fabric_secrets::seal_wifi_credential(
            "Phone hotspot",
            os_actions::WifiSecurity::Wpa2,
            "not-a-real-password",
            provider,
            [requester.clone()],
            crate::shell::device_fabric::FabricStamp {
                counter: 1,
                device: provider.clone(),
            },
        )
        .unwrap();
        Host {
            credential,
            sent: Vec::new(),
            hotspot_starts: 0,
            hotspot_stops: 0,
            fail_send_after: None,
            fail_hotspot: false,
            fail_connect: false,
            seal_error: None,
            complete_jobs: true,
            connects: Vec::new(),
            disconnected: Vec::new(),
            held: Vec::new(),
            finished: Vec::new(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tethering_runtime_starts_hotspot_only_on_consent_and_fails_closed_visibly() {
        fn explicit_consent_starts_owned_hotspot_and_policy_revocation_stops_it() {
            let _guard = test_lock();
            reset_shell_state();
            let requester = device(3);
            let provider = device(4);
            let request_id = [7; 16];
            let request = TetheringSignal::Request {
                protocol_version: crate::shell::personal_tethering::TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: requester.clone(),
                provider: provider.clone(),
                requester_label: "Laptop".into(),
                issued_unix_ms: 1_000,
                expires_unix_ms: 31_000,
            };
            let mut runtime = PersonalTetheringRuntime::default();
            let mut host = host(&requester, &provider);
            runtime.handle_signal(&mut host, &provider, &requester, &request, 1_000);
            assert_eq!(pending_tethering_consents().len(), 1);
            authorize_phone_tethering(request_id, true).unwrap();
            runtime.pump_with(&mut host, &provider, "Phone", true, 1_001);
            assert_eq!(host.hotspot_starts, 1);
            assert_eq!(host.sent.len(), 2);
            assert_eq!(host.hotspot_stops, 0);

            runtime.pump_with(&mut host, &provider, "Phone", false, 1_002);
            assert_eq!(host.hotspot_stops, 1);
            assert!(matches!(
                host.sent.last().unwrap().1,
                TetheringSignal::Stop { .. }
            ));
            assert!(pending_tethering_consents().is_empty());
            assert_eq!(
                tethering_statuses()[0].phase,
                TetheringPhase::PolicyDisabled
            );
        }

        fn failed_grant_delivery_tears_down_the_owned_hotspot() {
            let _guard = test_lock();
            reset_shell_state();
            let requester = device(5);
            let provider = device(6);
            let request_id = [8; 16];
            let request = TetheringSignal::Request {
                protocol_version: crate::shell::personal_tethering::TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: requester.clone(),
                provider: provider.clone(),
                requester_label: "Tablet".into(),
                issued_unix_ms: 2_000,
                expires_unix_ms: 32_000,
            };
            let mut runtime = PersonalTetheringRuntime::default();
            let mut host = host(&requester, &provider);
            runtime.handle_signal(&mut host, &provider, &requester, &request, 2_000);
            authorize_phone_tethering(request_id, true).unwrap();
            host.fail_send_after = Some(0);
            runtime.pump_with(&mut host, &provider, "Phone", true, 2_001);
            assert_eq!(host.hotspot_starts, 1);
            assert_eq!(host.hotspot_stops, 1);
            assert!(runtime.provider_leases.is_empty());
            assert_eq!(tethering_statuses()[0].phase, TetheringPhase::Failed);
        }

        fn deny_expiry_spoof_and_networkmanager_failure_are_visible_and_fail_closed() {
            let _guard = test_lock();
            reset_shell_state();
            let requester = device(7);
            let provider = device(8);
            let mut runtime = PersonalTetheringRuntime::default();
            let mut host = host(&requester, &provider);

            let request = |id, claimed: DeviceId, expires| TetheringSignal::Request {
                protocol_version: crate::shell::personal_tethering::TETHERING_PROTOCOL_VERSION,
                request_id: id,
                requester: claimed,
                provider: provider.clone(),
                requester_label: "Laptop".into(),
                issued_unix_ms: 1_000,
                expires_unix_ms: expires,
            };

            runtime.handle_signal(
                &mut host,
                &provider,
                &requester,
                &request([9; 16], device(9), 31_000),
                1_000,
            );
            assert!(pending_tethering_consents().is_empty(), "spoofed requester");

            runtime.handle_signal(
                &mut host,
                &provider,
                &requester,
                &request([10; 16], requester.clone(), 31_000),
                1_000,
            );
            authorize_phone_tethering([10; 16], false).unwrap();
            runtime.pump_with(&mut host, &provider, "Phone", true, 1_001);
            assert_eq!(tethering_statuses()[0].phase, TetheringPhase::Denied);
            assert_eq!(host.hotspot_starts, 0);

            runtime.handle_signal(
                &mut host,
                &provider,
                &requester,
                &request([11; 16], requester.clone(), 2_000),
                1_000,
            );
            runtime.pump_with(&mut host, &provider, "Phone", true, 2_001);
            assert!(tethering_statuses().iter().any(|status| {
                status.request_id == [11; 16] && status.phase == TetheringPhase::Expired
            }));

            runtime.handle_signal(
                &mut host,
                &provider,
                &requester,
                &request([12; 16], requester.clone(), 31_000),
                1_000,
            );
            authorize_phone_tethering([12; 16], true).unwrap();
            host.fail_hotspot = true;
            runtime.pump_with(&mut host, &provider, "Phone", true, 1_001);
            assert!(tethering_statuses().iter().any(|status| {
                status.request_id == [12; 16] && status.phase == TetheringPhase::Failed
            }));
            assert!(runtime.provider_leases.is_empty());
        }

        explicit_consent_starts_owned_hotspot_and_policy_revocation_stops_it();
        failed_grant_delivery_tears_down_the_owned_hotspot();
        deny_expiry_spoof_and_networkmanager_failure_are_visible_and_fail_closed();
    }

    #[test]
    fn tethering_runtime_disconnects_notify_and_neutralize_the_provider() {
        fn explicit_disconnect_notifies_peer_and_neutralizes_provider() {
            let _guard = test_lock();
            reset_shell_state();
            let requester = device(10);
            let provider = device(11);
            let request_id = [13; 16];
            let request = TetheringSignal::Request {
                protocol_version: crate::shell::personal_tethering::TETHERING_PROTOCOL_VERSION,
                request_id,
                requester: requester.clone(),
                provider: provider.clone(),
                requester_label: "Laptop".into(),
                issued_unix_ms: 1_000,
                expires_unix_ms: 31_000,
            };
            let mut runtime = PersonalTetheringRuntime::default();
            let mut host = host(&requester, &provider);
            runtime.handle_signal(&mut host, &provider, &requester, &request, 1_000);
            authorize_phone_tethering(request_id, true).unwrap();
            runtime.pump_with(&mut host, &provider, "Phone", true, 1_001);
            disconnect_phone_tethering(request_id).unwrap();
            runtime.pump_with(&mut host, &provider, "Phone", true, 1_002);
            assert_eq!(host.hotspot_stops, 1);
            assert!(matches!(
                host.sent.last().unwrap().1,
                TetheringSignal::Stop { .. }
            ));
            assert_eq!(tethering_statuses()[0].phase, TetheringPhase::Disconnected);
        }

        fn peer_loss_disconnects_networkmanager_and_reports_offline() {
            let _guard = test_lock();
            reset_shell_state();
            let requester = device(12);
            let provider = device(13);
            let request_id = [15; 16];
            let mut runtime = PersonalTetheringRuntime::default();
            let mut host = host(&requester, &provider);
            let request = runtime
                .protocol
                .request(
                    TetheringRoute::CertifiedPhone(provider.clone()),
                    &requester,
                    "Laptop",
                    request_id,
                    1_000,
                )
                .unwrap();
            shell_state().lock().unwrap().statuses.insert(
                request_id,
                TetheringStatusView {
                    request_id,
                    peer: provider.clone(),
                    peer_label: "Phone".into(),
                    direction: TetheringDirection::Requester,
                    phase: TetheringPhase::Active,
                    expires_unix_ms: signal_expiry(&request),
                },
            );
            runtime
                .requester_connections
                .insert(request_id, (provider.clone(), "Phone hotspot".into()));

            runtime.peer_unavailable(&mut host, &requester, &provider);
            assert_eq!(host.disconnected, vec!["Phone hotspot"]);
            assert_eq!(tethering_statuses()[0].phase, TetheringPhase::Offline);
        }

        explicit_disconnect_notifies_peer_and_neutralizes_provider();
        peer_loss_disconnects_networkmanager_and_reports_offline();
    }

    fn phase_of(request_id: TetheringId) -> TetheringPhase {
        tethering_statuses()
            .into_iter()
            .find(|status| status.request_id == request_id)
            .expect("a recorded status")
            .phase
    }

    /// A requester that asked `provider` for `request_id`, the phone that
    /// granted it, and the grant's decision and credential, not yet received.
    fn granted_requester(
        requester: &DeviceId,
        provider: &DeviceId,
        request_id: TetheringId,
    ) -> (
        PersonalTetheringRuntime,
        Host,
        PersonalTethering,
        Vec<TetheringSignal>,
    ) {
        let mut runtime = PersonalTetheringRuntime::default();
        let mut host = host(requester, provider);
        host.complete_jobs = false;
        let request = runtime
            .protocol
            .request(
                TetheringRoute::CertifiedPhone(provider.clone()),
                requester,
                "Laptop",
                request_id,
                1_000,
            )
            .unwrap();
        shell_state().lock().unwrap().statuses.insert(
            request_id,
            TetheringStatusView {
                request_id,
                peer: provider.clone(),
                peer_label: "Phone".into(),
                direction: TetheringDirection::Requester,
                phase: TetheringPhase::Requesting,
                expires_unix_ms: signal_expiry(&request),
            },
        );
        let mut phone = PersonalTethering::default();
        phone
            .receive_request(requester, provider, &request, 1_000)
            .unwrap();
        let grant = phone
            .decide(
                provider,
                request_id,
                true,
                Some([5; 16]),
                Some(host.credential.clone()),
                1_001,
            )
            .unwrap();
        (runtime, host, phone, grant)
    }

    /// The provider side of one consent prompt for `request_id`.
    fn consenting_provider(
        requester: &DeviceId,
        provider: &DeviceId,
        request_id: TetheringId,
    ) -> (PersonalTetheringRuntime, Host) {
        let request = TetheringSignal::Request {
            protocol_version: crate::shell::personal_tethering::TETHERING_PROTOCOL_VERSION,
            request_id,
            requester: requester.clone(),
            provider: provider.clone(),
            requester_label: "Laptop".into(),
            issued_unix_ms: 1_000,
            expires_unix_ms: 31_000,
        };
        let mut runtime = PersonalTetheringRuntime::default();
        let mut host = host(requester, provider);
        runtime.handle_signal(&mut host, provider, requester, &request, 1_000);
        (runtime, host)
    }

    #[test]
    fn credential_connect_is_queued_not_run_inline() {
        let _guard = test_lock();
        reset_shell_state();
        let requester = device(20);
        let provider = device(21);

        let joins = [21; 16];
        let (mut runtime, mut host, _phone, grant) =
            granted_requester(&requester, &provider, joins);
        for signal in &grant {
            runtime.handle_signal(&mut host, &requester, &provider, signal, 1_002);
        }
        assert_eq!(
            host.connects,
            vec![joins],
            "the join is queued for the worker"
        );
        assert!(
            runtime.requester_connections.is_empty(),
            "nothing is joined before NetworkManager answers"
        );
        assert_eq!(phase_of(joins), TetheringPhase::Connecting);
        host.release();
        runtime.pump_with(&mut host, &requester, "Laptop", true, 1_003);
        assert_eq!(phase_of(joins), TetheringPhase::Active);
        assert!(runtime.requester_connections.contains_key(&joins));
        assert!(runtime.pending_connects.is_empty());

        reset_shell_state();
        let fails = [22; 16];
        let (mut runtime, mut host, _phone, grant) =
            granted_requester(&requester, &provider, fails);
        host.fail_connect = true;
        for signal in &grant {
            runtime.handle_signal(&mut host, &requester, &provider, signal, 1_002);
        }
        assert!(host.sent.is_empty());
        host.release();
        runtime.pump_with(&mut host, &requester, "Laptop", true, 1_003);
        assert!(
            matches!(
                host.sent.last(),
                Some((target, TetheringSignal::Cancel { .. })) if target == &provider
            ),
            "a failed join cancels the grant on the phone"
        );
        assert_eq!(phase_of(fails), TetheringPhase::Failed);
        assert!(runtime.requester_connections.is_empty());
        assert!(runtime.pending_connects.is_empty());
    }

    #[test]
    fn stop_during_pending_connect_disconnects_on_completion() {
        let _guard = test_lock();
        reset_shell_state();
        let requester = device(22);
        let provider = device(23);

        let stopped = [23; 16];
        let (mut runtime, mut host, mut phone, grant) =
            granted_requester(&requester, &provider, stopped);
        for signal in &grant {
            runtime.handle_signal(&mut host, &requester, &provider, signal, 1_002);
        }
        let stop = phone.stop_provider_lease(&provider, stopped).unwrap();
        runtime.handle_signal(&mut host, &requester, &provider, &stop, 1_003);
        assert_eq!(
            host.disconnected,
            vec!["Phone hotspot"],
            "the disconnect is queued behind the join it undoes"
        );
        assert_eq!(phase_of(stopped), TetheringPhase::Disconnected);
        host.release();
        runtime.pump_with(&mut host, &requester, "Laptop", true, 1_004);
        assert!(
            runtime.requester_connections.is_empty(),
            "a join that finishes after its session ended is not recorded"
        );
        assert_eq!(phase_of(stopped), TetheringPhase::Disconnected);
        assert_eq!(host.disconnected.len(), 1);

        reset_shell_state();
        let lapses = [24; 16];
        let (mut runtime, mut host, _phone, grant) =
            granted_requester(&requester, &provider, lapses);
        for signal in &grant {
            runtime.handle_signal(&mut host, &requester, &provider, signal, 1_002);
        }
        let expires = signal_expiry(&grant[1]);
        runtime.pump_with(&mut host, &requester, "Laptop", true, expires);
        assert_eq!(host.disconnected, vec!["Phone hotspot"]);
        assert_eq!(phase_of(lapses), TetheringPhase::Expired);
        host.release();
        runtime.pump_with(
            &mut host,
            &requester,
            "Laptop",
            true,
            expires.saturating_add(1),
        );
        assert!(runtime.requester_connections.is_empty());
        assert_eq!(host.disconnected.len(), 1);
    }

    #[test]
    fn approve_queues_hotspot_and_seals_on_completion() {
        let _guard = test_lock();
        reset_shell_state();
        let requester = device(26);
        let provider = device(27);
        let request_id = [27; 16];
        let (mut runtime, mut host) = consenting_provider(&requester, &provider, request_id);
        host.complete_jobs = false;
        authorize_phone_tethering(request_id, true).unwrap();
        runtime.pump_with(&mut host, &provider, "Phone", true, 1_001);
        assert_eq!(host.hotspot_starts, 1);
        assert!(
            host.sent.is_empty(),
            "nothing is granted before the hotspot is up"
        );
        assert_eq!(phase_of(request_id), TetheringPhase::Preparing);
        assert!(runtime.provider_leases.is_empty());

        host.release();
        runtime.pump_with(&mut host, &provider, "Phone", true, 1_002);
        assert!(matches!(
            host.sent.as_slice(),
            [
                (_, TetheringSignal::Decision { accepted: true, .. }),
                (_, TetheringSignal::Credential { .. })
            ]
        ));
        assert_eq!(runtime.provider_leases.get(&request_id), Some(&requester));
        assert_eq!(phase_of(request_id), TetheringPhase::Active);
        assert_eq!(host.hotspot_stops, 0);
    }

    #[test]
    fn seal_failure_stops_hotspot() {
        let _guard = test_lock();
        reset_shell_state();
        let requester = device(28);
        let provider = device(29);
        let request_id = [29; 16];
        let (mut runtime, mut host) = consenting_provider(&requester, &provider, request_id);
        host.seal_error = Some("no certified identity to seal with");
        authorize_phone_tethering(request_id, true).unwrap();
        runtime.pump_with(&mut host, &provider, "Phone", true, 1_001);
        assert_eq!(host.hotspot_starts, 1);
        assert_eq!(
            host.hotspot_stops, 1,
            "a hotspot whose credential cannot be sealed comes down again"
        );
        assert!(matches!(
            host.sent.first(),
            Some((
                _,
                TetheringSignal::Decision {
                    accepted: false,
                    ..
                }
            ))
        ));
        assert!(runtime.provider_leases.is_empty());
        assert_eq!(phase_of(request_id), TetheringPhase::Failed);
    }

    #[test]
    fn disconnect_while_hotspot_starts_declines_and_stops_it() {
        let _guard = test_lock();
        reset_shell_state();
        let requester = device(30);
        let provider = device(31);
        let request_id = [31; 16];
        let (mut runtime, mut host) = consenting_provider(&requester, &provider, request_id);
        host.complete_jobs = false;
        authorize_phone_tethering(request_id, true).unwrap();
        runtime.pump_with(&mut host, &provider, "Phone", true, 1_001);
        disconnect_phone_tethering(request_id).unwrap();
        runtime.pump_with(&mut host, &provider, "Phone", true, 1_002);
        assert!(matches!(
            host.sent.as_slice(),
            [(
                _,
                TetheringSignal::Decision {
                    accepted: false,
                    ..
                }
            )]
        ));
        assert_eq!(phase_of(request_id), TetheringPhase::Disconnected);
        assert_eq!(host.hotspot_stops, 0, "the hotspot is still starting");

        host.release();
        runtime.pump_with(&mut host, &provider, "Phone", true, 1_003);
        assert_eq!(host.hotspot_stops, 1);
        assert_eq!(host.sent.len(), 1, "no grant follows the decline");
        assert!(runtime.provider_leases.is_empty());
        assert_eq!(phase_of(request_id), TetheringPhase::Disconnected);
    }
}
