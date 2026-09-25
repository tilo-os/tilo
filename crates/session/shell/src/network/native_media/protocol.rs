//! Closed native media protocol. Production views borrow the actual native
//! owner and HostIdentity; callers cannot supply a signing preimage.
use ed25519_dalek::{Signer, SigningKey};
use qos_peer_state::StatePeer;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};
use tilo_relay::delivery_v3::{
    auth::{self, Carrier},
    descriptor::{self, VerifiedDescriptor},
    http_proof, media_wire,
};
use tilo_user_identity::{DeviceCert, RevocationList};

/// Borrowed only from the live Peer and its acknowledged descriptor floor.
/// No field is supplied by a JS proof/identity object.
pub(super) struct OwnerView<'a> {
    pub peer: &'a StatePeer,
    pub key: &'a SigningKey,
    pub governor: &'a DeviceCert,
    pub host: &'a DeviceCert,
    pub device: &'a DeviceCert,
    pub workspace: u64,
    pub continuity: u64,
    pub generation: descriptor::GenerationExpectation,
    pub now_ns: u64,
}
impl OwnerView<'_> {
    /// An enrollment refresh replaces only an inert candidate, never the pins.
    pub fn validate_enrollment_hint(
        &self,
        workspace: u64,
        room: &str,
        governor: &DeviceCert,
        host: &DeviceCert,
        device: &DeviceCert,
        hint: &qos_peer_state::join::TransportHint,
    ) -> Result<(), String> {
        if workspace != self.workspace
            || room != self.peer.room()
            || governor != self.governor
            || host != self.host
            || device != self.device
        {
            return Err("media enrollment custody mismatch".into());
        }
        let descriptor =
            descriptor::verify_transport_hint(hint, &self.descriptor_authority()).map_err(fail)?;
        descriptor
            .client_scope(
                &self.descriptor_authority(),
                self.device,
                Carrier::Https,
                0,
                0,
            )
            .map_err(fail)?;
        Ok(())
    }

    fn descriptor_authority(&self) -> descriptor::DescriptorAuthority<'_> {
        descriptor::DescriptorAuthority {
            peer: self.peer,
            pinned_governor: self.governor,
            pinned_host: self.host,
            workspace: self.workspace,
            generation: self.generation,
            allowed_delivery_capabilities: tilo_relay::delivery_v3::capabilities::PRODUCT_DATA_V1,
            now: time(self.now_ns),
        }
    }
    fn revocations(&self) -> Result<RevocationList, String> {
        let mut revoked = RevocationList::new();
        for entry in self.peer.revocations() {
            revoked.insert(entry).map_err(|e| e.to_string())?;
        }
        Ok(revoked)
    }
}
impl auth::Authority for OwnerView<'_> {
    fn authorize(
        &self,
        binding: &auth::Binding,
        device: &DeviceCert,
        host: &DeviceCert,
        now_ns: u64,
    ) -> Result<(), auth::AuthError> {
        if self.peer.incomplete_reason().is_some()
            || self.peer.authority() != self.governor
            || binding.workspace != self.workspace
            || binding.workspace_root != self.governor.user_public_key
            || device != self.device
            || host != self.host
            || !host.capabilities.can_act_as_host
            || !self.peer.certificates().contains(device)
            || !self.peer.certificates().contains(host)
            || self
                .peer
                .permissions(device.device_public_key, time(now_ns))
                .is_err()
            || self
                .peer
                .permissions(host.device_public_key, time(now_ns))
                .is_err()
        {
            return Err(auth::AuthError::PolicyDenied);
        }
        Ok(())
    }
}
fn time(ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(ns)
}
fn fail(message: impl ToString) -> String {
    message.to_string()
}

/// Borrowed directly under the actual owner's current publication gate. No key.
#[derive(Clone, Copy)]
pub(super) struct CurrentView<'a> {
    pub peer: &'a StatePeer,
    pub governor: &'a DeviceCert,
    pub host: &'a DeviceCert,
    pub device: &'a DeviceCert,
    pub workspace: u64,
    pub continuity: u64,
    pub generation: descriptor::GenerationExpectation,
    pub now_ns: u64,
}
impl<'a> OwnerView<'a> {
    pub fn current_view(&self) -> CurrentView<'a> {
        CurrentView {
            peer: self.peer,
            governor: self.governor,
            host: self.host,
            device: self.device,
            workspace: self.workspace,
            continuity: self.continuity,
            generation: self.generation,
            now_ns: self.now_ns,
        }
    }
}
impl CurrentView<'_> {
    fn current_view(&self) -> Self {
        *self
    }
    fn descriptor_authority(&self) -> descriptor::DescriptorAuthority<'_> {
        descriptor::DescriptorAuthority {
            peer: self.peer,
            pinned_governor: self.governor,
            pinned_host: self.host,
            workspace: self.workspace,
            generation: self.generation,
            allowed_delivery_capabilities: tilo_relay::delivery_v3::capabilities::PRODUCT_DATA_V1,
            now: time(self.now_ns),
        }
    }
}
/// Exact actor-prepared bytes. The HTTP worker cannot choose a signing preimage.
pub(super) struct PreparedRequest {
    pub id: u32,
    pub origin: String,
    pub target: &'static str,
    pub body: Vec<u8>,
    pub proof: Option<Vec<u8>>,
}
pub(super) enum Step {
    Request(PreparedRequest),
    HttpReady,
    Grant(control::GrantView),
    Relay(control::RelayView),
    Presence(control::PresenceView),
    Refreshed,
    Left,
    RelayClosed,
}
struct HttpSession {
    proof: auth::ServerProof,
    nonce: [u8; 32],
    digest: [u8; 32],
}
enum Pending {
    Control {
        intent: u32,
        stage: control::ControlStage,
    },
    Server {
        nonce: [u8; 32],
    },
    Ready {
        proof: auth::ServerProof,
        nonce: [u8; 32],
        digest: [u8; 32],
    },
}
pub(super) struct NativeControl {
    continuity: u64,
    identity: (u64, String, DeviceCert, DeviceCert, DeviceCert),
    descriptor: VerifiedDescriptor,
    pending: BTreeMap<u32, Pending>,
    next: u32,
    session: Option<HttpSession>,
    closed: bool,
    participation: Option<control::Participation>,
    ending: Option<control::Participation>,
}
impl NativeControl {
    pub fn new(
        view: &OwnerView<'_>,
        hint: &qos_peer_state::join::TransportHint,
    ) -> Result<Self, String> {
        if view.key.verifying_key().to_bytes() != view.device.device_public_key
            || !matches!(
                view.generation,
                descriptor::GenerationExpectation::Current { .. }
            )
        {
            return Err("native media requires acknowledged current custody".into());
        }
        let descriptor =
            descriptor::verify_transport_hint(hint, &view.descriptor_authority()).map_err(fail)?;
        if descriptor.media_capabilities() & descriptor::MEDIA_VOICE_V1 == 0 {
            return Err("native voice is not installed".into());
        }
        let value = Self {
            continuity: view.continuity,
            identity: (
                view.workspace,
                view.peer.room().into(),
                view.governor.clone(),
                view.host.clone(),
                view.device.clone(),
            ),
            descriptor,
            pending: BTreeMap::new(),
            next: 0,
            session: None,
            closed: false,
            participation: None,
            ending: None,
        };
        value.check(view)?;
        Ok(value)
    }
    fn check_current(&self, view: &CurrentView<'_>) -> Result<(), String> {
        if self.closed
            || self.continuity != view.continuity
            || self.identity.0 != view.workspace
            || self.identity.1 != view.peer.room()
            || &self.identity.2 != view.governor
            || &self.identity.3 != view.host
            || &self.identity.4 != view.device
            || !matches!(
                view.generation,
                descriptor::GenerationExpectation::Current { .. }
            )
        {
            return Err("native media custody changed".into());
        }
        self.descriptor
            .revalidate(&view.descriptor_authority())
            .map_err(fail)?;
        self.descriptor
            .client_scope(
                &view.descriptor_authority(),
                view.device,
                Carrier::Https,
                0,
                0,
            )
            .map_err(fail)?;
        Ok(())
    }
    fn check(&self, view: &OwnerView<'_>) -> Result<(), String> {
        if view.key.verifying_key().to_bytes() != view.device.device_public_key {
            return Err("native signing custody changed".into());
        }
        self.check_current(&view.current_view())
    }
    fn request(
        &mut self,
        target: &'static str,
        body: Vec<u8>,
        proof: Option<Vec<u8>>,
        pending: Pending,
    ) -> Result<PreparedRequest, String> {
        if self.pending.len() >= 8 {
            return Err(fail("media control capacity"));
        }
        let id = self
            .next
            .checked_add(1)
            .ok_or("media operation counter exhausted")?;
        self.next = id;
        self.pending.insert(id, pending);
        Ok(PreparedRequest {
            id,
            origin: self.descriptor.origin().into(),
            target,
            body,
            proof,
        })
    }
    pub fn start_http(&mut self, view: &OwnerView<'_>) -> Result<PreparedRequest, String> {
        self.check(view)?;
        if self.session.is_some() || !self.pending.is_empty() {
            return Err(fail("media HTTP handshake already active"));
        }
        let nonce = media_wire::fresh_nonce().map_err(fail)?;
        let body =
            postcard::to_stdvec(&(3_u16, view.device.device_public_key, nonce, 0_u64, 0_u64))
                .map_err(fail)?;
        self.request("/peer/v3/challenge", body, None, Pending::Server { nonce })
    }
    fn verify_host(
        &self,
        view: &OwnerView<'_>,
        proof: &auth::ServerProof,
        nonce: [u8; 32],
    ) -> Result<auth::VerifiedHost, String> {
        let revoked = view.revocations()?;
        let scope = self
            .descriptor
            .client_scope(
                &view.descriptor_authority(),
                view.device,
                Carrier::Https,
                0,
                0,
            )
            .map_err(fail)?;
        auth::verify_server(
            &auth::ClientExpectations {
                scope,
                capabilities: auth::CapabilityPolicy {
                    supported: 3,
                    allowed: self.descriptor.delivery_capabilities(),
                },
                credentials: auth::Credentials {
                    device: view.device,
                    host: view.host,
                    device_revocations: &revoked,
                    host_revocations: &revoked,
                },
                authority: view,
                now_ns: view.now_ns,
            },
            nonce,
            proof,
        )
        .map_err(fail)
    }
    pub fn advance(&mut self, view: &OwnerView<'_>, id: u32, bytes: &[u8]) -> Result<Step, String> {
        self.check(view)?;
        let pending = self
            .pending
            .remove(&id)
            .ok_or("unknown or consumed media operation")?;
        if bytes.len() > 8192 {
            return Err(fail("oversized media response"));
        }
        match pending {
            Pending::Control { intent, stage } => {
                if self.ending.as_ref().is_some_and(|p| p.id == intent) {
                    let live = self.participation.take();
                    self.participation = self.ending.take();
                    let result = self.advance_control(view, intent, stage, bytes);
                    self.ending = self.participation.take();
                    self.participation = live;
                    result
                } else {
                    self.advance_control(view, intent, stage, bytes)
                }
            }
            Pending::Server { nonce } => {
                let proof = auth::ServerProof::decode(bytes).map_err(fail)?;
                let verified = self.verify_host(view, &proof, nonce)?;
                let transcript = &proof.transcript;
                let digest = http_proof::session_binding_digest(
                    &transcript.binding,
                    transcript.session_id,
                    transcript.negotiated_capabilities,
                    transcript.expires_at_ns,
                )
                .map_err(fail)?;
                let body = verified
                    .client_proof(view.key)
                    .map_err(fail)?
                    .encode()
                    .map_err(fail)?;
                Ok(Step::Request(self.request(
                    "/peer/v3/sessions",
                    body,
                    None,
                    Pending::Ready {
                        proof,
                        nonce,
                        digest,
                    },
                )?))
            }
            Pending::Ready {
                proof,
                nonce,
                digest,
            } => {
                self.verify_host(view, &proof, nonce)?;
                let actual: (u16, [u8; 16], [u8; 32], u64, u64) =
                    postcard::from_bytes(bytes).map_err(fail)?;
                let expected = (
                    3,
                    proof.transcript.session_id,
                    digest,
                    proof.transcript.expires_at_ns,
                    proof.transcript.negotiated_capabilities,
                );
                if actual != expected || postcard::to_stdvec(&actual).map_err(fail)? != bytes {
                    return Err(fail("media HTTP ready binding mismatch"));
                }
                self.session = Some(HttpSession {
                    proof,
                    nonce,
                    digest,
                });
                Ok(Step::HttpReady)
            }
        }
    }
    pub fn forget(&mut self, id: u32) {
        self.pending.remove(&id);
    }
    /// Local retirement follows synchronous transport close. This cannot issue
    /// or extend authority and never reuses the old single-use proof.
    pub fn retire_carrier(&mut self, carrier: u32) {
        if let Some(part) = &mut self.participation {
            part.relays.remove(&carrier);
            part.relay_expires.remove(&carrier);
        }
    }
    pub fn current(&self, view: &CurrentView<'_>) -> bool {
        self.check_current(view).is_ok()
            && self
                .session
                .as_ref()
                .is_some_and(|session| view.now_ns < session.proof.transcript.expires_at_ns)
    }
    pub fn supported(&self, view: &CurrentView<'_>) -> bool {
        self.current(view) && self.descriptor.media_capabilities() & descriptor::MEDIA_VOICE_V1 != 0
    }
    /// Fresh owner validation of an already issued carrier; no signing key is
    /// borrowed or exported on the transport/audio current path.
    pub fn current_carrier(
        &self,
        view: &CurrentView<'_>,
        handle: u32,
        carrier: u32,
    ) -> Result<GrantView, String> {
        if !self.current(view) {
            return Err("native media HTTP custody expired".into());
        }
        let part = self
            .participation
            .as_ref()
            .filter(|part| part.id == handle && !part.leaving)
            .ok_or("native media participation ended")?;
        let (epoch, revision, directions) = control::policy(view, &part.request)?;
        let context = part
            .context
            .as_ref()
            .ok_or("native media context missing")?;
        let grant = part.grant.as_ref().ok_or("native media grant missing")?;
        if context.epoch != epoch
            || context.membership_generation != revision
            || context.directions != directions
            || grant.metadata.epoch != epoch
            || grant.metadata.membership_generation != revision
            || !part.relays.contains_key(&carrier)
            || part
                .relay_expires
                .get(&carrier)
                .is_none_or(|expiry| *expiry <= view.now_ns)
        {
            return Err("native media carrier authority expired".into());
        }
        self.grant_view(view)
    }
    pub fn http_ready(&self) -> bool {
        !self.closed && self.session.is_some()
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.pending.clear();
        self.session = None;
        self.participation = None;
        self.ending = None;
    }
}
#[path = "control.rs"]
mod control;
pub(super) use control::{GrantView, PresenceView, RelayView};
