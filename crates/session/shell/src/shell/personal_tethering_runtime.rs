//! Shell-owned runtime for consented certified-phone tethering.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, OnceLock};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use zeroize::Zeroizing;

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
            TetheringPhase::Requesting | TetheringPhase::ConsentRequired | TetheringPhase::Active
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
    if !crate::shell::continuity_policy::ContinuityPolicySnapshot::load()
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
    let request_id = random_id();
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

pub(crate) trait TetheringHost {
    fn send(&mut self, target: &DeviceId, signal: TetheringSignal) -> Result<(), String>;
    fn start_hotspot(
        &mut self,
        requester: &DeviceId,
    ) -> Result<crate::shell::device_fabric::WifiCredential, String>;
    fn stop_hotspot(&mut self);
    fn connect(
        &mut self,
        credential: &crate::shell::device_fabric::WifiCredential,
    ) -> Result<(), String>;
    fn disconnect(&mut self, ssid: &str);
}

struct ProductionHost<'a> {
    network: &'a ShellNetwork,
}

impl TetheringHost for ProductionHost<'_> {
    fn send(&mut self, target: &DeviceId, signal: TetheringSignal) -> Result<(), String> {
        self.network
            .send_personal_fabric_action(target, PersonalFabricAction::Tethering { signal })
            .map(|_| ())
    }

    fn start_hotspot(
        &mut self,
        requester: &DeviceId,
    ) -> Result<crate::shell::device_fabric::WifiCredential, String> {
        let (_, label) = self
            .network
            .local_device_fabric_identity()
            .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
        let suffix = label
            .chars()
            .filter(|c| !c.is_control())
            .take(20)
            .collect::<String>();
        let ssid = format!("Tilo {suffix}");
        let mut secret = [0_u8; 16];
        UnwrapErr(SysRng).fill_bytes(&mut secret);
        let password = Zeroizing::new(hex::encode(secret));
        let config = os_actions::HotspotConfiguration {
            ssid: ssid.clone(),
            password: password.to_string(),
            band: os_actions::HotspotBand::Automatic,
        };
        os_actions::configure_hotspot(&config)?;
        match self
            .network
            .seal_tethering_credential(requester, &ssid, &password)
        {
            Ok(credential) => Ok(credential),
            Err(error) => {
                let _ = os_actions::set_hotspot(false);
                Err(error)
            }
        }
    }

    fn stop_hotspot(&mut self) {
        let _ = os_actions::set_hotspot(false);
    }

    fn connect(
        &mut self,
        credential: &crate::shell::device_fabric::WifiCredential,
    ) -> Result<(), String> {
        let password = Zeroizing::new(self.network.open_tethering_credential(credential)?);
        os_actions::connect_wifi(&credential.ssid, Some(&password))
    }

    fn disconnect(&mut self, ssid: &str) {
        let _ = os_actions::disconnect_wifi(ssid);
    }
}

#[derive(Default)]
pub(crate) struct PersonalTetheringRuntime {
    protocol: PersonalTethering,
    provider_leases: BTreeMap<TetheringId, DeviceId>,
    requester_connections: BTreeMap<TetheringId, (DeviceId, String)>,
    online_peers: BTreeSet<DeviceId>,
}

impl PersonalTetheringRuntime {
    pub(crate) fn pump(&mut self, network: &ShellNetwork, permitted: bool, now_unix_ms: u64) {
        let Some((local, label)) = network.local_device_fabric_identity() else {
            return;
        };
        let mut host = ProductionHost { network };
        self.pump_with(&mut host, &local, &label, permitted, now_unix_ms);
        let online = network
            .device_fabric()
            .snapshots()
            .values()
            .filter(|snapshot| snapshot.presence.online)
            .map(|snapshot| snapshot.owner.clone())
            .collect::<BTreeSet<_>>();
        let previous = std::mem::take(&mut self.online_peers);
        for peer in previous.difference(&online) {
            self.peer_unavailable(&mut host, &local, peer);
        }
        self.online_peers = online;
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
        let mut host = ProductionHost { network };
        self.handle_signal(&mut host, &local, &envelope.source, signal, now_unix_ms);
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
            if let Some((_, ssid)) = self.requester_connections.remove(&request_id) {
                host.disconnect(&ssid);
            }
        }
        let had_provider_lease = !self.provider_leases.is_empty();
        for stop in self.protocol.expire(local, now_unix_ms) {
            self.provider_leases.remove(&signal_id(&stop));
            let target = signal_target(&stop);
            let _ = host.send(&target, stop);
        }
        if had_provider_lease && self.provider_leases.is_empty() {
            host.stop_hotspot();
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
                    let approved = approved && self.provider_leases.is_empty();
                    let credential = if approved {
                        if let Ok(credential) = host.start_hotspot(&consent.requester) {
                            Some(credential)
                        } else {
                            set_phase(request_id, TetheringPhase::Failed, 0);
                            None
                        }
                    } else {
                        None
                    };
                    let accepted = credential.is_some();
                    let nonce = accepted.then(random_id);
                    let Ok(signals) = self.protocol.decide(
                        local,
                        request_id,
                        accepted,
                        nonce,
                        credential,
                        now_unix_ms,
                    ) else {
                        if approved {
                            host.stop_hotspot();
                        }
                        continue;
                    };
                    let mut sent = true;
                    for signal in signals {
                        if host.send(&consent.requester, signal).is_err() {
                            sent = false;
                            break;
                        }
                    }
                    if accepted && sent {
                        self.provider_leases.insert(request_id, consent.requester);
                        set_phase(
                            request_id,
                            TetheringPhase::Active,
                            now_unix_ms
                                .saturating_add(crate::shell::personal_tethering::GRANT_TTL_MS),
                        );
                    } else if approved {
                        host.stop_hotspot();
                        for stop in self.protocol.peer_unavailable(local, &consent.requester) {
                            let _ = host.send(&consent.requester, stop);
                        }
                        set_phase(request_id, TetheringPhase::Failed, 0);
                    } else {
                        set_phase(request_id, TetheringPhase::Denied, 0);
                    }
                }
                Command::Disconnect { request_id } => {
                    if self.provider_leases.remove(&request_id).is_some() {
                        if let Ok(stop) = self.protocol.stop_provider_lease(local, request_id) {
                            let target = signal_target(&stop);
                            let _ = host.send(&target, stop);
                        }
                        host.stop_hotspot();
                    } else if let Ok(cancel) = self.protocol.cancel_request(local, request_id) {
                        if let Some((_, ssid)) = self.requester_connections.remove(&request_id) {
                            host.disconnect(&ssid);
                        }
                        let target = signal_target(&cancel);
                        let _ = host.send(&target, cancel);
                    }
                    set_phase(request_id, TetheringPhase::Disconnected, 0);
                }
            }
        }
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
                if let Ok(TetheringEvent::ConnectOffered {
                    request_id,
                    credential,
                    ..
                }) = self
                    .protocol
                    .receive_credential(source, local, signal, now_unix_ms)
                {
                    if host.connect(&credential).is_ok() {
                        self.requester_connections
                            .insert(request_id, (source.clone(), credential.ssid));
                        set_phase(request_id, TetheringPhase::Active, signal_expiry(signal));
                    } else {
                        if let Ok(cancel) = self.protocol.cancel_request(local, request_id) {
                            let _ = host.send(source, cancel);
                        }
                        set_phase(request_id, TetheringPhase::Failed, 0);
                    }
                }
            }
            TetheringSignal::Stop { .. } => {
                if let Ok(TetheringEvent::Ended { request_id, .. }) =
                    self.protocol.receive_stop(source, local, signal)
                    && let Some((_, ssid)) = self.requester_connections.remove(&request_id)
                {
                    host.disconnect(&ssid);
                    set_phase(request_id, TetheringPhase::Disconnected, 0);
                }
            }
            TetheringSignal::Cancel { .. } => {
                if let Ok(TetheringEvent::Ended { request_id, .. }) =
                    self.protocol.receive_cancel(source, local, signal)
                {
                    let was_active = self.provider_leases.remove(&request_id).is_some();
                    if was_active {
                        host.stop_hotspot();
                    }
                    if let Ok(mut state) = shell_state().lock() {
                        state.consents.remove(&request_id);
                    }
                    set_phase(request_id, TetheringPhase::Disconnected, 0);
                }
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
            .map(|(request_id, (_, ssid))| (*request_id, ssid.clone()))
            .collect::<Vec<_>>();
        for (request_id, ssid) in ended {
            self.requester_connections.remove(&request_id);
            host.disconnect(&ssid);
        }
        if had_provider_lease && self.provider_leases.is_empty() {
            host.stop_hotspot();
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
            host.stop_hotspot();
        }
        for (_, (_, ssid)) in std::mem::take(&mut self.requester_connections) {
            host.disconnect(&ssid);
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

fn random_id() -> [u8; 16] {
    let mut value = [0; 16];
    while value == [0; 16] {
        UnwrapErr(SysRng).fill_bytes(&mut value);
    }
    value
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
        disconnected: Vec<String>,
    }

    impl TetheringHost for Host {
        fn send(&mut self, target: &DeviceId, signal: TetheringSignal) -> Result<(), String> {
            if self.fail_send_after == Some(self.sent.len()) {
                return Err("transport full".into());
            }
            self.sent.push((target.clone(), signal));
            Ok(())
        }

        fn start_hotspot(
            &mut self,
            _requester: &DeviceId,
        ) -> Result<crate::shell::device_fabric::WifiCredential, String> {
            self.hotspot_starts = self.hotspot_starts.saturating_add(1);
            if self.fail_hotspot {
                return Err("NetworkManager refused hotspot".into());
            }
            Ok(self.credential.clone())
        }

        fn stop_hotspot(&mut self) {
            self.hotspot_stops = self.hotspot_stops.saturating_add(1);
        }

        fn connect(
            &mut self,
            _credential: &crate::shell::device_fabric::WifiCredential,
        ) -> Result<(), String> {
            if self.fail_connect {
                Err("NetworkManager refused connection".into())
            } else {
                Ok(())
            }
        }

        fn disconnect(&mut self, ssid: &str) {
            self.disconnected.push(ssid.to_string());
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
            disconnected: Vec::new(),
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
}
