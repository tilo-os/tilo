//! Device-possession custody for the existing serialized collaboration service.
//! This layer never issues invitations, admits membership, or creates room state.
use crate::{
    SessionOwner,
    committed_authority::CommittedView,
    host_bytes::{ByteCredit, HostBytes},
};
use anyhow::{Result, ensure};
use qos_peer_state::join::JoinTransportRequest;
use tilo_relay::delivery_v3::{
    capabilities::PRODUCT_DATA_V1,
    onboarding_wire::{OnboardingOperation, OnboardingRequest, OnboardingScope},
};
use tilo_user_identity::DeviceCert;
mod input;
mod local;
mod payload;
mod reply;

const NONCE_LIMIT: usize = 16_384;
#[derive(Debug)]
struct CapacityUnavailable;
impl std::fmt::Display for CapacityUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("onboarding capacity unavailable")
    }
}
impl std::error::Error for CapacityUnavailable {}
fn unavailable() -> anyhow::Error {
    CapacityUnavailable.into()
}
fn rejection_status(error: &anyhow::Error) -> hyper::StatusCode {
    if error.is::<CapacityUnavailable>() {
        hyper::StatusCode::SERVICE_UNAVAILABLE
    } else {
        hyper::StatusCode::FORBIDDEN
    }
}
struct SeenNonce {
    device: [u8; 32],
    nonce: [u8; 32],
    expires: u64,
}
pub(crate) struct OnboardingRegistry {
    boot: [u8; 16],
    high_water: u64,
    nonces: Vec<SeenNonce>,
    pins: Vec<reply::Pinned>,
    budget: HostBytes,
    // Retained decoded cache drops before its exact capacity credit.
    _nonce_credit: ByteCredit,
    _pin_credit: ByteCredit,
}
struct Admission {
    certificate: DeviceCert,
    request: Option<JoinTransportRequest>,
    operation: OnboardingOperation,
    request_digest: [u8; 32],
    nonce: [u8; 32],
    expires: u64,
    descriptor_digest: [u8; 32],
    owner_generation: u64,
    owner_identity: [u8; 32],
    _decode_credit: ByteCredit,
}
/// Original charged HTTP input, deadline and proof remain together until the
/// actual owner service and reconciliation finish. No raw endpoint constructor.
pub(crate) struct OnboardingExchange {
    admission: Admission,
    request: crate::https::BufferedRequest,
}
pub(crate) struct OnboardingOutcome {
    pub(crate) response: qos_peer_state::join::JoinTransportResponse,
    pub(crate) reconcile: bool,
}
impl OnboardingExchange {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.request.is_cancelled()
    }
    /// Invoke only inside the actual serialized owner scope. Keeping the owned
    /// DTO here prevents its exact decode credit from being detached from it.
    pub(crate) fn invoke(
        &mut self,
        owner: &mut SessionOwner,
        service: &crate::collaboration::CollaborationService,
        now: u64,
    ) -> Result<OnboardingOutcome> {
        use qos_peer_state::join::{JoinInspection, JoinTransportResponse as Response};
        ensure!(!self.is_cancelled(), "onboarding exchange cancelled");
        ensure!(
            now < self.admission.expires,
            "onboarding proof expired before invocation"
        );
        ensure!(
            owner.committed_generation() == self.admission.owner_generation
                && owner.committed_peer_identity() == self.admission.owner_identity,
            "onboarding owner changed before invocation"
        );
        let request = self
            .admission
            .request
            .take()
            .ok_or_else(|| anyhow::anyhow!("onboarding invocation already taken"))?;
        let remote = self.admission.certificate.device_public_key;
        let mut reconcile = false;
        let result = match request {
            JoinTransportRequest::Inspect { signed_invitation } => service
                .inspect(owner, &signed_invitation, now)
                .map(|verified| {
                    Response::Inspection(JoinInspection {
                        invite_id: verified.invite_id(),
                        grant: verified.grant().clone(),
                    })
                }),
            JoinTransportRequest::Challenge(request) => service
                .challenge(owner, remote, request, now)
                .map(Response::Challenge),
            JoinTransportRequest::Complete(request) => {
                service.complete(owner, remote, request, now).map(|done| {
                    reconcile = true;
                    Response::Complete(done.bootstrap)
                })
            }
            JoinTransportRequest::MembershipChallenge(request) => service
                .membership_challenge(owner, remote, request, now)
                .map(Response::MembershipChallenge),
            JoinTransportRequest::MembershipComplete(request) => service
                .membership_complete(owner, remote, request, now)
                .map(|done| {
                    reconcile = true;
                    Response::MembershipComplete(done.bootstrap)
                }),
        };
        Ok(OnboardingOutcome {
            response: result.unwrap_or_else(|error| Response::Error {
                code: 403,
                message: error.to_string(),
            }),
            reconcile,
        })
    }
}
impl OnboardingRegistry {
    /// Host maintenance calls this even when no onboarding request is queued.
    /// Expiry releases only this registry's pinned response custody.
    pub(crate) fn maintain(&mut self, now: u64) -> Result<()> {
        ensure!(now >= self.high_water, "onboarding clock regressed");
        self.high_water = now;
        self.nonces.retain(|entry| entry.expires > now);
        self.pins.retain(|pin| pin.expires > now);
        Ok(())
    }
    pub(crate) fn new(boot: [u8; 16], budget: HostBytes) -> Result<Self> {
        ensure!(boot != [0; 16], "onboarding boot unavailable");
        let credit = budget
            .try_acquire_bulk(NONCE_LIMIT * size_of::<SeenNonce>())
            .ok_or_else(unavailable)?;
        let mut nonces = Vec::new();
        nonces.try_reserve_exact(NONCE_LIMIT)?;
        ensure!(nonces.capacity() == NONCE_LIMIT, "nonce capacity changed");
        let pin_credit = budget
            .try_acquire_bulk(8 * size_of::<reply::Pinned>())
            .ok_or_else(unavailable)?;
        let mut pins = Vec::new();
        pins.try_reserve_exact(8)?;
        ensure!(pins.capacity() == 8, "pin capacity changed");
        Ok(Self {
            boot,
            high_water: 0,
            nonces,
            pins,
            budget,
            _nonce_credit: credit,
            _pin_credit: pin_credit,
        })
    }
    pub(crate) fn admit(
        &mut self,
        owner: &SessionOwner,
        view: &CommittedView,
        request: crate::https::BufferedRequest,
        now: u64,
    ) -> Result<OnboardingExchange> {
        ensure!(!request.is_cancelled(), "onboarding request cancelled");
        let result = if request.method().as_str() == "POST" {
            self.verify(owner, view, request.target(), request.body(), now)
        } else {
            Err(anyhow::anyhow!("onboarding requires POST"))
        };
        match result {
            Ok(admission) => Ok(OnboardingExchange { admission, request }),
            Err(error) => {
                let _ = request.respond(rejection_status(&error), b"onboarding denied".to_vec());
                Err(error)
            }
        }
    }
    fn verify(
        &mut self,
        owner: &SessionOwner,
        view: &CommittedView,
        target: &str,
        body: &[u8],
        now: u64,
    ) -> Result<Admission> {
        ensure!(now >= self.high_water, "onboarding clock regressed");
        self.high_water = now;
        self.nonces.retain(|entry| entry.expires > now);
        let wire = OnboardingRequest::decode(body)?;
        ensure!(
            wire.operation == OnboardingOperation::from_target("POST", target)?,
            "onboarding route mismatch"
        );
        let certificate = input::certificate(wire.certificate)?;
        let request = input::Request::decode(wire.body)?;
        let heap = certificate
            .heap()
            .checked_add(request.heap())
            .ok_or_else(|| anyhow::anyhow!("join allocation overflow"))?;
        let credit = self.budget.try_acquire_bulk(heap).ok_or_else(unavailable)?;
        // Existing certificate verification serializes a certificate no larger
        // than the preflighted 4096 bytes; retain its scratch before it runs.
        let _verification = self.budget.try_acquire_bulk(8192).ok_or_else(unavailable)?;
        let certificate = certificate.into_owned();
        tilo_user_identity::verify_device_cert_at(&certificate, now)?;
        ensure!(
            !owner
                .peer()
                .revocations()
                .iter()
                .any(|entry| entry.revoked_device_public_key == certificate.device_public_key),
            "onboarding device revoked"
        );
        let descriptor = owner.verified_delivery_transport(view, at(now), PRODUCT_DATA_V1)?;
        wire.verify_device_signature(
            OnboardingScope {
                origin: descriptor.origin(),
                host: owner.host_certificate().device_public_key,
                workspace: owner.workspace_id(),
                root: owner.peer().authority().user_public_key,
                room: owner.peer().room().parse()?,
            },
            &certificate,
            now,
        )?;
        if self.nonces.len() == NONCE_LIMIT {
            return Err(unavailable());
        }
        ensure!(
            !self
                .nonces
                .iter()
                .any(|entry| entry.device == certificate.device_public_key
                    && entry.nonce == wire.nonce),
            "onboarding nonce replay"
        );
        let request = request.into_owned();
        // The service still makes the actual invitation/current membership
        // decision. This only prevents substituting another transport identity.
        let same_device = match &request {
            JoinTransportRequest::Inspect { .. } => true,
            JoinTransportRequest::Challenge(v) => v.device_cert == certificate,
            JoinTransportRequest::Complete(v) => {
                v.proof.challenge.device_public_key == certificate.device_public_key
                    && v.proof.challenge.user_public_key == certificate.user_public_key
            }
            JoinTransportRequest::MembershipChallenge(v) => v.device_cert == certificate,
            JoinTransportRequest::MembershipComplete(v) => {
                v.proof.challenge.device_public_key == certificate.device_public_key
                    && v.proof.challenge.user_public_key == certificate.user_public_key
            }
        };
        ensure!(same_device, "join proof names another device");
        self.nonces.push(SeenNonce {
            device: certificate.device_public_key,
            nonce: wire.nonce,
            expires: wire.expires_at_ns,
        });
        Ok(Admission {
            certificate,
            request: Some(request),
            operation: wire.operation,
            request_digest: wire.request_digest()?,
            nonce: wire.nonce,
            expires: wire.expires_at_ns,
            descriptor_digest: descriptor.descriptor_digest(),
            owner_generation: owner.committed_generation(),
            owner_identity: owner.committed_peer_identity(),
            _decode_credit: credit,
        })
    }
}
fn at(now: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now)
}

#[cfg(test)]
mod tests;
