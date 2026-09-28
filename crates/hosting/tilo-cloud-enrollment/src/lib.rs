//! Closed hosted-enrollment protocol between a browser owner and a Tilo Cloud
//! Store. Custody is serialized material; each step checkpoints it before the
//! next remote operation.
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use serde::Deserialize;
use tilo_peer_state::join::{
    self, JoinTransportRequest as Request, JoinTransportResponse as Response,
};
use tilo_user_identity as identity;
mod material;
use material::{
    CLOCK_TOLERANCE_NS, MAX_APPLY_RETRIES, MAX_JOIN, MAX_MATERIAL, MAX_REQUEST, MaterialV4, Phase,
    PhaseView, verify_certificate,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostedAttemptError {
    Material,
    Scope,
    Expired,
    Checkpoint,
    Phase,
    Cancelled,
    Capacity,
    Response,
    Signature,
    /// The Store is bound to another owner root; this identity cannot enroll it.
    Claimed,
}
type Result<T> = std::result::Result<T, HostedAttemptError>;
pub struct Selection {
    pub account_id: String,
    pub machine_id: String,
}
pub struct Checkpoint<'a> {
    pub material: &'a [u8],
}
pub struct ApplyRequest<'a> {
    pub operation_key: uuid::Uuid,
    pub payload: &'a [u8],
}
/// Unqualified DTO custody only. No StatePeer/admitted/ready conversion exists.
pub struct PendingHostedBootstrap {
    _held: tilo_peer_state::resources::BoundedJoinResponse,
}
struct Saved {
    material: MaterialV4,
    bytes: Vec<u8>,
}
pub struct HostedAttempt {
    current: Saved,
    staged: Option<Saved>,
    cancelled: bool,
    completed: bool,
}
impl HostedAttempt {
    pub fn restore(bytes: &[u8], selected: Selection, now: u64) -> Result<Self> {
        let material: MaterialV4 = material::read_json(bytes)?;
        material.validate(now)?;
        if material.account_id != selected.account_id || material.machine_id != selected.machine_id
        {
            return Err(HostedAttemptError::Scope);
        }
        Ok(Self {
            current: Saved {
                material,
                bytes: bytes.to_vec(),
            },
            staged: None,
            cancelled: false,
            completed: false,
        })
    }
    fn live(&self) -> Result<()> {
        if self.cancelled {
            Err(HostedAttemptError::Cancelled)
        } else if self.completed {
            Err(HostedAttemptError::Phase)
        } else {
            Ok(())
        }
    }
    fn ready(&self) -> Result<()> {
        self.live()?;
        if self.staged.is_some() {
            return Err(HostedAttemptError::Checkpoint);
        }
        Ok(())
    }
    fn stage(&self, phase: PhaseView<'_>, now: u64) -> Result<Saved> {
        self.stage_view(self.current.material.with_phase(phase), now)
    }
    fn stage_view(&self, view: material::MaterialView<'_>, now: u64) -> Result<Saved> {
        let bytes = material::encode_json(&view, MAX_MATERIAL)?;
        let material: MaterialV4 = material::read_json(&bytes)?;
        material.validate(now)?;
        Ok(Saved { material, bytes })
    }
    fn checkpoint(&self) -> Checkpoint<'_> {
        Checkpoint {
            material: &self.staged.as_ref().expect("staged checkpoint").bytes,
        }
    }
    fn enrollment(&self) -> Result<identity::cloud_enrollment::EnrollmentRequest> {
        let (bytes, _) = self
            .current
            .material
            .phase
            .enrollment()
            .ok_or(HostedAttemptError::Phase)?;
        material::read_json(bytes)
    }
    pub fn prepare_enrollment(&mut self, bytes: &[u8], now: u64) -> Result<Checkpoint<'_>> {
        self.live()?;
        if !matches!(self.current.material.phase, Phase::Initial) {
            return Err(HostedAttemptError::Phase);
        }
        let challenge: identity::cloud_enrollment::Challenge = material::read_json(bytes)?;
        if let Some(staged) = &self.staged {
            let Some((bytes, _)) = staged.material.phase.enrollment() else {
                return Err(HostedAttemptError::Phase);
            };
            let request: identity::cloud_enrollment::EnrollmentRequest =
                material::read_json(bytes)?;
            if request.challenge != challenge {
                return Err(HostedAttemptError::Scope);
            }
            return Ok(self.checkpoint());
        }
        let candidate = &self.current.material;
        candidate.validate(now)?;
        candidate.check_challenge(&challenge, now)?;
        // check_challenge admitted either this root's stable host certificate
        // or an unbound Store for explicit first-claim custody.
        let host_certificate = match &challenge.host_certificate {
            Some(host) => host.clone(),
            None => candidate.first_host_certificate(&challenge)?,
        };
        let mut request = identity::cloud_enrollment::EnrollmentRequest {
            version: 1,
            host_certificate,
            browser_certificate: candidate.certificate.clone(),
            challenge: challenge.clone(),
            root_signature: Vec::new(),
            device_signature: Vec::new(),
        };
        identity::cloud_enrollment::sign_request(
            &mut request,
            &SigningKey::from_bytes(&candidate.root_secret),
            &candidate.device(),
        )
        .map_err(|_| HostedAttemptError::Signature)?;
        let bytes = material::encode_json(&request, MAX_REQUEST)?;
        let staged = self.stage(
            PhaseView::Apply {
                enrollment: &bytes,
                signed_at_ns: now,
            },
            now,
        )?;
        self.staged = Some(staged);
        Ok(self.checkpoint())
    }
    pub fn acknowledge(&mut self, saved: &[u8], now: u64) -> Result<()> {
        self.live()?;
        let staged = self.staged.as_ref().ok_or(HostedAttemptError::Checkpoint)?;
        if staged.bytes != saved {
            return Err(HostedAttemptError::Checkpoint);
        }
        // Exact byte ACK refers to this already-parsed immutable value.
        // Fresh validation precedes moving custody; there is no third copy.
        staged.material.validate(now)?;
        let staged = self.staged.take().ok_or(HostedAttemptError::Checkpoint)?;
        self.current = staged;
        Ok(())
    }
    pub fn apply_request(&self) -> Result<ApplyRequest<'_>> {
        self.ready()?;
        let Phase::Apply { enrollment, .. } = &self.current.material.phase else {
            return Err(HostedAttemptError::Phase);
        };
        Ok(ApplyRequest {
            operation_key: self.current.material.operation_id()?,
            payload: enrollment,
        })
    }
    pub fn accept_enrollment(&mut self, bytes: &[u8], now: u64) -> Result<Checkpoint<'_>> {
        self.live()?;
        if !matches!(self.current.material.phase, Phase::Apply { .. }) {
            return Err(HostedAttemptError::Phase);
        }
        let enrollment = self.enrollment()?;
        let reply: EnrollmentReply = material::read_json(bytes)?;
        reply.verify(&enrollment, now)?;
        if self.staged.is_some() {
            return Ok(self.checkpoint());
        }
        let candidate = &self.current.material;
        candidate.validate(now)?;
        let request = Request::MembershipChallenge(join::MembershipChallengeRequest {
            room: enrollment.challenge.room.clone(),
            workspace_id: enrollment
                .challenge
                .workspace_id
                .parse()
                .map_err(|_| HostedAttemptError::Scope)?,
            device_cert: candidate.certificate.clone(),
            request_id: candidate.request_id()?,
        });
        let encoded = material::encode_postcard(&request, MAX_JOIN)?;
        let Phase::Apply {
            enrollment,
            signed_at_ns,
        } = &candidate.phase
        else {
            return Err(HostedAttemptError::Phase);
        };
        let staged = self.stage(
            PhaseView::MembershipChallenge {
                enrollment,
                signed_at_ns: *signed_at_ns,
                request: &encoded,
            },
            now,
        )?;
        self.staged = Some(staged);
        Ok(self.checkpoint())
    }

    pub fn membership_challenge_request(&self) -> Result<&[u8]> {
        self.ready()?;
        let Phase::MembershipChallenge { request, .. } = &self.current.material.phase else {
            return Err(HostedAttemptError::Phase);
        };
        Ok(request)
    }
    pub fn prepare_membership(&mut self, bytes: &[u8], now: u64) -> Result<Checkpoint<'_>> {
        self.live()?;
        if !matches!(
            self.current.material.phase,
            Phase::MembershipChallenge { .. }
        ) {
            return Err(HostedAttemptError::Phase);
        }
        let response = bounded_reply(bytes)?;
        let Response::MembershipChallenge(response) = response.response() else {
            return Err(HostedAttemptError::Response);
        };
        if let Some(staged) = &self.staged {
            let Phase::MembershipComplete { proof, .. } = &staged.material.phase else {
                return Err(HostedAttemptError::Phase);
            };
            let held = material::read_request(proof)?;
            let Request::MembershipComplete(proof) = &held else {
                return Err(HostedAttemptError::Phase);
            };
            if proof.proof.challenge != response.challenge {
                return Err(HostedAttemptError::Scope);
            }
            return Ok(self.checkpoint());
        }
        let candidate = &self.current.material;
        candidate.validate(now)?;
        let enrollment = self.enrollment()?;
        candidate.check_membership(
            &response.challenge,
            &enrollment.host_certificate,
            &enrollment.challenge,
            now,
        )?;
        let proof = join::sign_membership_proof(response.challenge.clone(), &candidate.device())
            .map_err(|_| HostedAttemptError::Signature)?;
        let encoded = material::encode_postcard(
            &Request::MembershipComplete(join::MembershipCompleteRequest { proof }),
            MAX_JOIN,
        )?;
        let Phase::MembershipChallenge {
            enrollment,
            signed_at_ns,
            request,
        } = &candidate.phase
        else {
            return Err(HostedAttemptError::Phase);
        };
        let staged = self.stage(
            PhaseView::MembershipComplete {
                enrollment,
                signed_at_ns: *signed_at_ns,
                request,
                proof: &encoded,
                proof_at_ns: now,
            },
            now,
        )?;
        self.staged = Some(staged);
        Ok(self.checkpoint())
    }

    pub fn membership_complete_request(&self) -> Result<&[u8]> {
        self.ready()?;
        let Phase::MembershipComplete { proof, .. } = &self.current.material.phase else {
            return Err(HostedAttemptError::Phase);
        };
        Ok(proof)
    }
    pub fn accept_membership(&mut self, bytes: &[u8], now: u64) -> Result<PendingHostedBootstrap> {
        self.membership_complete_request()?;
        self.current.material.validate(now)?;
        let held = bounded_reply(bytes)?;
        let Response::OwnerMembershipComplete(reply) = held.response() else {
            return Err(HostedAttemptError::Response);
        };
        let enrollment = self.enrollment()?;
        self.owner_reply_scope(reply, &enrollment)?;
        self.completed = true;
        Ok(PendingHostedBootstrap { _held: held })
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }
    fn owner_reply_scope(
        &self,
        reply: &join::OwnerBootstrapV1,
        enrollment: &identity::cloud_enrollment::EnrollmentRequest,
    ) -> Result<()> {
        if reply.version != 1
            || reply.device_cert != self.current.material.certificate
            || reply.authority != enrollment.host_certificate
            || reply.room != enrollment.challenge.room
            || reply.workspace_id
                != enrollment
                    .challenge
                    .workspace_id
                    .parse::<u64>()
                    .map_err(|_| HostedAttemptError::Scope)?
            || reply.generation == 0
        {
            return Err(HostedAttemptError::Scope);
        }
        Ok(())
    }
}

/// Next remote operation named only by durable custody.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostedStep {
    Inspect,
    Apply,
    Enrolled,
}
/// Scope of a Store that accepted this device's verified enrollment reply.
pub struct EnrolledScope {
    pub room: String,
    pub workspace: u64,
    pub host: identity::DeviceCert,
    pub certificate: identity::DeviceCert,
}

// Owner continuation after the verified Store reply.
impl HostedAttempt {
    /// Fresh custody for an unbound Store: root, device and attempt keys are
    /// generated here and never leave serialized custody.
    pub fn create_material(selected: &Selection, label: &str, now: u64) -> Result<Vec<u8>> {
        if label.trim().is_empty() || label.len() > 128 || now == 0 {
            return Err(HostedAttemptError::Material);
        }
        // Issue the fresh identity slightly in the past so a Store whose clock
        // is up to the tolerance behind this browser accepts the certificates.
        let issued = now.saturating_sub(CLOCK_TOLERANCE_NS).max(1);
        let (root, _) = identity::UserIdentity::generate(label, issued);
        let (device, _) = identity::DeviceIdentity::generate(label, issued);
        let (attempt, _) = identity::DeviceIdentity::generate("Cloud enrollment attempt", issued);
        let mut certificate = identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: identity::DeviceCapabilities::member(),
            issued_at_ns: issued,
            expires_at_ns: None,
            signature: [0; 64],
        };
        identity::sign_device_cert(&mut certificate, &root)
            .map_err(|_| HostedAttemptError::Signature)?;
        let material = MaterialV4 {
            version: 4,
            kind: "cloud-owner".into(),
            account_id: selected.account_id.clone(),
            machine_id: selected.machine_id.clone(),
            root_secret: root.to_bytes(),
            device_secret: device.to_bytes(),
            attempt_id: attempt.to_bytes(),
            created_at_ns: issued,
            certificate,
            phase: Phase::Initial,
            first_claim: true,
            apply_retry: 0,
        };
        material.validate(now)?;
        let bytes = material::encode_json(&material, MAX_MATERIAL)?;
        // Round-trip through the one restore reader before custody is offered.
        let restored = Self::restore(
            &bytes,
            Selection {
                account_id: selected.account_id.clone(),
                machine_id: selected.machine_id.clone(),
            },
            now,
        )?;
        drop(restored);
        Ok(bytes)
    }
    /// Store.apply was refused for this signed request (lost first-claim
    /// race, expired or replaced challenge, certificate not yet valid). Stage
    /// a return to Inspect with the same keys and a fresh apply operation
    /// key. Like every checkpoint it moves custody only after its exact ACK.
    pub fn reset_apply(&mut self, now: u64) -> Result<Checkpoint<'_>> {
        self.live()?;
        if !matches!(self.current.material.phase, Phase::Apply { .. }) {
            return Err(HostedAttemptError::Phase);
        }
        if let Some(staged) = &self.staged {
            if matches!(staged.material.phase, Phase::Initial) {
                return Ok(self.checkpoint());
            }
            return Err(HostedAttemptError::Checkpoint);
        }
        let retry = self
            .current
            .material
            .apply_retry
            .checked_add(1)
            .filter(|retry| *retry <= MAX_APPLY_RETRIES)
            .ok_or(HostedAttemptError::Capacity)?;
        let view = self
            .current
            .material
            .with_phase_retry(PhaseView::Initial, retry);
        let staged = self.stage_view(view, now)?;
        self.staged = Some(staged);
        Ok(self.checkpoint())
    }
    pub fn step(&self) -> Result<HostedStep> {
        self.ready()?;
        Ok(match &self.current.material.phase {
            Phase::Initial => HostedStep::Inspect,
            Phase::Apply { .. } => HostedStep::Apply,
            Phase::MembershipChallenge { .. } | Phase::MembershipComplete { .. } => {
                HostedStep::Enrolled
            }
        })
    }
    pub fn certificate(&self) -> &identity::DeviceCert {
        &self.current.material.certificate
    }
    pub fn attempt_binding(&self) -> ([u8; 32], u64) {
        (
            self.current.material.attempt_id,
            self.current.material.created_at_ns,
        )
    }
    pub fn current_material(&self) -> &[u8] {
        &self.current.bytes
    }
    fn enrolled_enrollment(&self) -> Result<identity::cloud_enrollment::EnrollmentRequest> {
        if self.step()? != HostedStep::Enrolled {
            return Err(HostedAttemptError::Phase);
        }
        self.enrollment()
    }
    pub fn enrolled(&self) -> Result<EnrolledScope> {
        let enrollment = self.enrolled_enrollment()?;
        let certificate = &self.current.material.certificate;
        Ok(EnrolledScope {
            room: enrollment.challenge.room.clone(),
            workspace: enrollment
                .challenge
                .workspace_id
                .parse()
                .map_err(|_| HostedAttemptError::Scope)?,
            host: enrollment.host_certificate.clone(),
            certificate: certificate.clone(),
        })
    }
    /// The enrolled owner root signs only bilateral machine-mesh approvals.
    pub fn mesh_root_key(&self) -> Result<SigningKey> {
        self.enrolled_enrollment()?;
        Ok(SigningKey::from_bytes(&self.current.material.root_secret))
    }
    /// The enrolled device key signs only closed onboarding wrappers/proofs.
    pub fn device_key(&self) -> Result<SigningKey> {
        self.enrolled_enrollment()?;
        Ok(self.current.material.device())
    }
    /// A membership refresh uses a fresh request identifier; it never reuses
    /// the checkpointed first exchange whose challenge may have expired.
    pub fn refresh_challenge(&self, request_id: [u8; 32]) -> Result<Request> {
        if request_id == [0; 32] {
            return Err(HostedAttemptError::Scope);
        }
        let enrollment = self.enrolled_enrollment()?;
        let certificate = &self.current.material.certificate;
        Ok(Request::MembershipChallenge(
            join::MembershipChallengeRequest {
                room: enrollment.challenge.room.clone(),
                workspace_id: enrollment
                    .challenge
                    .workspace_id
                    .parse()
                    .map_err(|_| HostedAttemptError::Scope)?,
                device_cert: certificate.clone(),
                request_id,
            },
        ))
    }
    pub fn refresh_proof(
        &self,
        challenge: &join::MembershipChallenge,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<Request> {
        let enrollment = self.enrolled_enrollment()?;
        let candidate = &self.current.material;
        candidate.validate(now)?;
        candidate.check_membership_for(
            challenge,
            &enrollment.host_certificate,
            &enrollment.challenge,
            request_id,
            now,
        )?;
        let proof = join::sign_membership_proof(challenge.clone(), &candidate.device())
            .map_err(|_| HostedAttemptError::Signature)?;
        Ok(Request::MembershipComplete(
            join::MembershipCompleteRequest { proof },
        ))
    }
    /// Scope of an owner refresh reply. Authority is still established only by
    /// admitting its signed governance and deriving the structural Owner.
    pub fn check_owner_bootstrap(&self, reply: &join::OwnerBootstrapV1, now: u64) -> Result<()> {
        let enrollment = self.enrolled_enrollment()?;
        self.current.material.validate(now)?;
        self.owner_reply_scope(reply, &enrollment)
    }
}

fn bounded_reply(bytes: &[u8]) -> Result<tilo_peer_state::resources::BoundedJoinResponse> {
    use tilo_delivery_protocol::delivery_v3::onboarding_wire::MAX_RESPONSE_BYTES;
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_BYTES {
        return Err(HostedAttemptError::Response);
    }
    tilo_peer_state::resources::BoundedJoinResponse::decode(
        bytes,
        tilo_peer_state::resources::OuterLimits {
            allocation_bytes: 32 * 1024 * 1024,
            collection_entries: MAX_RESPONSE_BYTES,
        },
    )
    .map_err(|_| HostedAttemptError::Capacity)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentReply {
    version: u32,
    certificate: identity::DeviceCert,
    root_public_key: [u8; 32],
    host_certificate: identity::DeviceCert,
    revocations: Vec<identity::RevocationEntry>,
    bootstrap: EnrollmentRoom,
    // These relay routes are inert and are never passed to a transport.
    #[serde(rename = "relay_address")]
    _relay: serde::de::IgnoredAny,
    #[serde(rename = "packet_relay")]
    _packet: serde::de::IgnoredAny,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentRoom {
    room: String,
    workspace_id: String,
    directory_update: identity::DirectoryUpdate,
    directory_author: identity::DeviceCert,
    certified_roster: Vec<identity::DeviceCert>,
}
impl EnrollmentReply {
    fn verify(
        &self,
        request: &identity::cloud_enrollment::EnrollmentRequest,
        now: u64,
    ) -> Result<()> {
        if self.version != 1
            || self.certificate != request.browser_certificate
            || self.host_certificate != request.host_certificate
            || self.root_public_key != request.browser_certificate.user_public_key
            || self.bootstrap.room != request.challenge.room
            || self.bootstrap.workspace_id != request.challenge.workspace_id
            || self.bootstrap.directory_author != request.host_certificate
            || self.bootstrap.certified_roster.len() != 2
            || !self
                .bootstrap
                .certified_roster
                .contains(&request.host_certificate)
            || !self
                .bootstrap
                .certified_roster
                .contains(&request.browser_certificate)
        {
            return Err(HostedAttemptError::Scope);
        }
        for certificate in [&self.certificate, &self.host_certificate] {
            verify_certificate(certificate, now)?;
        }
        for revocation in &self.revocations {
            identity::verify_revocation_entry(revocation)
                .map_err(|_| HostedAttemptError::Signature)?;
            if revocation.user_public_key != self.root_public_key
                || (revocation.revoked_at_ns <= now
                    && [
                        self.certificate.device_public_key,
                        self.host_certificate.device_public_key,
                    ]
                    .contains(&revocation.revoked_device_public_key))
            {
                return Err(HostedAttemptError::Scope);
            }
        }
        let update = &self.bootstrap.directory_update;
        // The Store authored this update on its own clock.
        if update.author_device_public_key != self.host_certificate.device_public_key
            || update.authored_at_ns > now.saturating_add(CLOCK_TOLERANCE_NS)
        {
            return Err(HostedAttemptError::Scope);
        }
        // Verify only the existing signature transcript. Do not parse/apply the
        // Automerge delta or infer directory membership/readiness from this DTO.
        let digest = identity::canonical::signing_digest(
            identity::directory::DIR_DELTA_PREIMAGE_TAG,
            &(
                &update.author_device_public_key,
                update.authored_at_ns,
                update.delta.as_slice(),
            ),
        )
        .map_err(|_| HostedAttemptError::Signature)?;
        VerifyingKey::from_bytes(&update.author_device_public_key)
            .map_err(|_| HostedAttemptError::Signature)?
            .verify(&digest, &Signature::from_bytes(&update.signature))
            .map_err(|_| HostedAttemptError::Signature)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests;
