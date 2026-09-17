//! Shell-owned runtime for consented certified-phone tethering.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, OnceLock};

use rand_core::{OsRng, RngCore};
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
}

#[derive(Default)]
struct ShellState {
    commands: VecDeque<Command>,
    consents: BTreeMap<TetheringId, TetheringConsentView>,
}

fn shell_state() -> &'static Mutex<ShellState> {
    static STATE: OnceLock<Mutex<ShellState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(ShellState::default()))
}

#[must_use]
pub fn pending_tethering_consents() -> Vec<TetheringConsentView> {
    shell_state().lock().map_or_else(
        |_| Vec::new(),
        |state| state.consents.values().cloned().collect(),
    )
}

/// Queue a request only for an online certified phone. Local-hotspot control
/// remains the existing Quick Settings NetworkManager action.
pub fn request_phone_tethering(
    network: &ShellNetwork,
    provider: &DeviceId,
) -> Result<TetheringId, String> {
    let snapshot = network
        .device_fabric()
        .snapshots()
        .get(provider)
        .cloned()
        .filter(|snapshot| snapshot.presence.online && snapshot.presence.kind == DeviceKind::Phone)
        .ok_or_else(|| "choose an online certified phone".to_string())?;
    let _ = snapshot;
    let request_id = random_id();
    shell_state()
        .lock()
        .map_err(|_| "tethering coordinator is unavailable".to_string())?
        .commands
        .push_back(Command::Request {
            request_id,
            provider: provider.clone(),
        });
    Ok(request_id)
}

pub fn authorize_phone_tethering(request_id: TetheringId, approved: bool) -> Result<(), String> {
    let mut state = shell_state()
        .lock()
        .map_err(|_| "tethering coordinator is unavailable".to_string())?;
    if !state.consents.contains_key(&request_id) {
        return Err("that tethering request is no longer pending".into());
    }
    state.commands.push_back(Command::Decide {
        request_id,
        approved,
    });
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
            .ok_or_else(|| "this device has no certified personal identity".to_string())?;
        let suffix = label
            .chars()
            .filter(|c| !c.is_control())
            .take(20)
            .collect::<String>();
        let ssid = format!("Tilo {suffix}");
        let mut secret = [0_u8; 16];
        OsRng.fill_bytes(&mut secret);
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
        for peer in self
            .online_peers
            .difference(&online)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.peer_unavailable(&mut host, &local, &peer);
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
                    ) && host.send(&provider, signal).is_err()
                    {
                        self.protocol.peer_unavailable(local, &provider);
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
                        host.start_hotspot(&consent.requester).ok()
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
                    if approved && sent {
                        self.provider_leases.insert(request_id, consent.requester);
                    } else if approved {
                        host.stop_hotspot();
                        for stop in self.protocol.peer_unavailable(local, &consent.requester) {
                            let _ = host.send(&consent.requester, stop);
                        }
                    }
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
                    state.consents.insert(
                        request_id,
                        TetheringConsentView {
                            request_id,
                            requester,
                            requester_label,
                        },
                    );
                }
            }
            TetheringSignal::Decision { .. } => {
                let _ = self
                    .protocol
                    .receive_decision(source, local, signal, now_unix_ms);
            }
            TetheringSignal::Credential { .. } => {
                if let Ok(TetheringEvent::ConnectOffered {
                    request_id,
                    credential,
                    ..
                }) = self
                    .protocol
                    .receive_credential(source, local, signal, now_unix_ms)
                    && host.connect(&credential).is_ok()
                {
                    self.requester_connections
                        .insert(request_id, (source.clone(), credential.ssid));
                }
            }
            TetheringSignal::Stop { .. } => {
                if let Ok(TetheringEvent::Ended { request_id, .. }) =
                    self.protocol.receive_stop(source, local, signal)
                    && let Some((_, ssid)) = self.requester_connections.remove(&request_id)
                {
                    host.disconnect(&ssid);
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
        }
    }
}

fn signal_target(signal: &TetheringSignal) -> DeviceId {
    match signal {
        TetheringSignal::Request { provider, .. } => provider.clone(),
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
        | TetheringSignal::Stop { request_id, .. } => *request_id,
    }
}

fn random_id() -> [u8; 16] {
    let mut value = [0; 16];
    while value == [0; 16] {
        OsRng.fill_bytes(&mut value);
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

    struct Host {
        credential: crate::shell::device_fabric::WifiCredential,
        sent: Vec<(DeviceId, TetheringSignal)>,
        hotspot_starts: usize,
        hotspot_stops: usize,
        fail_send_after: Option<usize>,
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
            self.hotspot_starts += 1;
            Ok(self.credential.clone())
        }

        fn stop_hotspot(&mut self) {
            self.hotspot_stops += 1;
        }

        fn connect(
            &mut self,
            _credential: &crate::shell::device_fabric::WifiCredential,
        ) -> Result<(), String> {
            Ok(())
        }

        fn disconnect(&mut self, _ssid: &str) {}
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
        }
    }

    #[test]
    fn explicit_consent_starts_owned_hotspot_and_policy_revocation_stops_it() {
        let _guard = test_lock();
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
    }

    #[test]
    fn failed_grant_delivery_tears_down_the_owned_hotspot() {
        let _guard = test_lock();
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
    }
}
