//! Portable closed protocol, also exercised against the actual Cloud Store.
use super::{identity, relay};
use automerge::resource_limits::NativeAllowance;
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use qos_peer_state::join::{
    self, JoinTransportRequest as Request, JoinTransportResponse as Response,
};
use serde::Deserialize;
#[path = "material.rs"]
mod material;
use material::{
    Bytes, CLOCK_TOLERANCE_NS, Held, MAX_APPLY_RETRIES, MAX_JOIN, MAX_MATERIAL, MAX_REQUEST,
    MaterialV4, Phase, PhaseView, allocation, verify_certificate,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HostedAttemptError {
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
pub(super) struct Selection {
    pub account_id: String,
    pub machine_id: String,
}
pub(super) struct Checkpoint<'a> {
    pub material: &'a [u8],
}
pub(super) struct ApplyRequest<'a> {
    pub operation_key: uuid::Uuid,
    pub payload: &'a [u8],
}
/// Unqualified DTO custody only. No StatePeer/admitted/ready conversion exists.
pub(super) struct PendingHostedBootstrap {
    _held: qos_peer_state::resources::BoundedJoinResponse,
}
struct Saved {
    material: Held<MaterialV4>,
    bytes: Bytes,
}
pub(super) struct HostedAttempt {
    current: Saved,
    staged: Option<Saved>,
    budget: NativeAllowance,
    cancelled: bool,
    completed: bool,
}
impl HostedAttempt {
    pub(super) fn restore(
        bytes: &[u8],
        selected: Selection,
        budget: &NativeAllowance,
        now: u64,
    ) -> Result<Self> {
        let material: Held<MaterialV4> = material::read_json(bytes, budget)?;
        material.value.validate(now, budget)?;
        if material.value.account_id != selected.account_id
            || material.value.machine_id != selected.machine_id
        {
            return Err(HostedAttemptError::Scope);
        }
        Ok(Self {
            current: Saved {
                material,
                bytes: Bytes::copy(bytes, budget)?,
            },
            staged: None,
            budget: budget.clone(),
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
        self.stage_view(self.current.material.value.with_phase(phase), now)
    }
    fn stage_view(&self, view: material::MaterialView<'_>, now: u64) -> Result<Saved> {
        let bytes = material::encode_json(&view, MAX_MATERIAL, &self.budget)?;
        let material: Held<MaterialV4> = material::read_json(&bytes.value, &self.budget)?;
        material.value.validate(now, &self.budget)?;
        Ok(Saved { material, bytes })
    }
    fn checkpoint(&self) -> Checkpoint<'_> {
        Checkpoint {
            material: &self.staged.as_ref().expect("staged checkpoint").bytes.value,
        }
    }
    fn enrollment(&self) -> Result<Held<identity::cloud_enrollment::EnrollmentRequest>> {
        let (bytes, _) = self
            .current
            .material
            .value
            .phase
            .enrollment()
            .ok_or(HostedAttemptError::Phase)?;
        material::read_json(bytes, &self.budget)
    }
    pub(super) fn prepare_enrollment(&mut self, bytes: &[u8], now: u64) -> Result<Checkpoint<'_>> {
        self.live()?;
        if !matches!(self.current.material.value.phase, Phase::Initial) {
            return Err(HostedAttemptError::Phase);
        }
        let challenge: Held<identity::cloud_enrollment::Challenge> =
            material::read_json(bytes, &self.budget)?;
        if let Some(staged) = &self.staged {
            let Some((bytes, _)) = staged.material.value.phase.enrollment() else {
                return Err(HostedAttemptError::Phase);
            };
            let request: Held<identity::cloud_enrollment::EnrollmentRequest> =
                material::read_json(bytes, &self.budget)?;
            if request.value.challenge != challenge.value {
                return Err(HostedAttemptError::Scope);
            }
            return Ok(self.checkpoint());
        }
        let candidate = &self.current.material.value;
        candidate.validate(now, &self.budget)?;
        candidate.check_challenge(&challenge.value, now, &self.budget)?;
        let _errors = allocation::helper_errors(&self.budget)?;
        // check_challenge admitted either this root's stable host certificate
        // or an unbound Store for explicit first-claim custody.
        let first = match &challenge.value.host_certificate {
            Some(_) => None,
            None => Some(candidate.first_host_certificate(&challenge.value, &self.budget)?),
        };
        let host = first
            .as_ref()
            .or(challenge.value.host_certificate.as_ref())
            .ok_or(HostedAttemptError::Scope)?;
        let _request = allocation::enrollment_backing(
            &challenge.value,
            host,
            &candidate.certificate,
            &self.budget,
        )?;
        let host_certificate = match first {
            Some(host) => host,
            None => challenge
                .value
                .host_certificate
                .clone()
                .ok_or(HostedAttemptError::Scope)?,
        };
        let mut request = identity::cloud_enrollment::EnrollmentRequest {
            version: 1,
            host_certificate,
            browser_certificate: candidate.certificate.clone(),
            challenge: challenge.value.clone(),
            root_signature: Vec::new(),
            device_signature: Vec::new(),
        };
        let _transcript = allocation::enrollment_transcript(&request, &self.budget)?;
        identity::cloud_enrollment::sign_request(
            &mut request,
            &SigningKey::from_bytes(&candidate.root_secret),
            &candidate.device(),
        )
        .map_err(|_| HostedAttemptError::Signature)?;
        let bytes = material::encode_json(&request, MAX_REQUEST, &self.budget)?;
        let staged = self.stage(
            PhaseView::Apply {
                enrollment: &bytes.value,
                signed_at_ns: now,
            },
            now,
        )?;
        self.staged = Some(staged);
        Ok(self.checkpoint())
    }
    pub(super) fn acknowledge(&mut self, saved: &[u8], now: u64) -> Result<()> {
        self.live()?;
        let staged = self.staged.as_ref().ok_or(HostedAttemptError::Checkpoint)?;
        if staged.bytes.value != saved {
            return Err(HostedAttemptError::Checkpoint);
        }
        // Exact byte ACK refers to this already-parsed immutable value.
        // Fresh validation precedes moving custody; there is no third copy.
        staged.material.value.validate(now, &self.budget)?;
        let staged = self.staged.take().ok_or(HostedAttemptError::Checkpoint)?;
        self.current = staged;
        Ok(())
    }
    pub(super) fn apply_request(&self) -> Result<ApplyRequest<'_>> {
        self.ready()?;
        let Phase::Apply { enrollment, .. } = &self.current.material.value.phase else {
            return Err(HostedAttemptError::Phase);
        };
        Ok(ApplyRequest {
            operation_key: self.current.material.value.operation_id(&self.budget)?,
            payload: enrollment,
        })
    }
    pub(super) fn accept_enrollment(&mut self, bytes: &[u8], now: u64) -> Result<Checkpoint<'_>> {
        self.live()?;
        if !matches!(self.current.material.value.phase, Phase::Apply { .. }) {
            return Err(HostedAttemptError::Phase);
        }
        let enrollment = self.enrollment()?;
        let reply: Held<EnrollmentReply> = material::read_json(bytes, &self.budget)?;
        reply.value.verify(&enrollment.value, now, &self.budget)?;
        if self.staged.is_some() {
            return Ok(self.checkpoint());
        }
        let candidate = &self.current.material.value;
        candidate.validate(now, &self.budget)?;
        let _request = allocation::reserve(
            &self.budget,
            allocation::sum(&[
                enrollment.value.challenge.room.len(),
                candidate.certificate.device_label.len(),
            ])?,
        )?;
        let request = Request::MembershipChallenge(join::MembershipChallengeRequest {
            room: enrollment.value.challenge.room.clone(),
            workspace_id: enrollment
                .value
                .challenge
                .workspace_id
                .parse()
                .map_err(|_| HostedAttemptError::Scope)?,
            device_cert: candidate.certificate.clone(),
            request_id: candidate.request_id(&self.budget)?,
        });
        let encoded = material::encode_postcard(&request, MAX_JOIN, &self.budget)?;
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
                request: &encoded.value,
            },
            now,
        )?;
        self.staged = Some(staged);
        Ok(self.checkpoint())
    }

    pub(super) fn membership_challenge_request(&self) -> Result<&[u8]> {
        self.ready()?;
        let Phase::MembershipChallenge { request, .. } = &self.current.material.value.phase else {
            return Err(HostedAttemptError::Phase);
        };
        Ok(request)
    }
    pub(super) fn prepare_membership(&mut self, bytes: &[u8], now: u64) -> Result<Checkpoint<'_>> {
        self.live()?;
        if !matches!(
            self.current.material.value.phase,
            Phase::MembershipChallenge { .. }
        ) {
            return Err(HostedAttemptError::Phase);
        }
        let response = bounded_reply(bytes, &self.budget)?;
        let Response::MembershipChallenge(response) = response.response() else {
            return Err(HostedAttemptError::Response);
        };
        if let Some(staged) = &self.staged {
            let Phase::MembershipComplete { proof, .. } = &staged.material.value.phase else {
                return Err(HostedAttemptError::Phase);
            };
            let held = material::read_request(proof, &self.budget)?;
            let Request::MembershipComplete(proof) = &held.value else {
                return Err(HostedAttemptError::Phase);
            };
            if proof.proof.challenge != response.challenge {
                return Err(HostedAttemptError::Scope);
            }
            return Ok(self.checkpoint());
        }
        let candidate = &self.current.material.value;
        candidate.validate(now, &self.budget)?;
        let enrollment = self.enrollment()?;
        candidate.check_membership(
            &response.challenge,
            &enrollment.value.host_certificate,
            &enrollment.value.challenge,
            now,
            &self.budget,
        )?;
        let _errors = allocation::helper_errors(&self.budget)?;
        let _proof = allocation::reserve(&self.budget, response.challenge.room.len())?;
        let _transcript = allocation::transcript(&response.challenge, &self.budget)?;
        let proof = join::sign_membership_proof(response.challenge.clone(), &candidate.device())
            .map_err(|_| HostedAttemptError::Signature)?;
        let encoded = material::encode_postcard(
            &Request::MembershipComplete(join::MembershipCompleteRequest { proof }),
            MAX_JOIN,
            &self.budget,
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
                proof: &encoded.value,
                proof_at_ns: now,
            },
            now,
        )?;
        self.staged = Some(staged);
        Ok(self.checkpoint())
    }

    pub(super) fn membership_complete_request(&self) -> Result<&[u8]> {
        self.ready()?;
        let Phase::MembershipComplete { proof, .. } = &self.current.material.value.phase else {
            return Err(HostedAttemptError::Phase);
        };
        Ok(proof)
    }
    pub(super) fn accept_membership(
        &mut self,
        bytes: &[u8],
        now: u64,
    ) -> Result<PendingHostedBootstrap> {
        self.membership_complete_request()?;
        self.current.material.value.validate(now, &self.budget)?;
        let held = bounded_reply(bytes, &self.budget)?;
        let Response::OwnerMembershipComplete(reply) = held.response() else {
            return Err(HostedAttemptError::Response);
        };
        let enrollment = self.enrollment()?;
        self.owner_reply_scope(reply, &enrollment.value)?;
        self.completed = true;
        Ok(PendingHostedBootstrap { _held: held })
    }
    pub(super) fn cancel(&mut self) {
        self.cancelled = true;
    }
    fn owner_reply_scope(
        &self,
        reply: &join::OwnerBootstrapV1,
        enrollment: &identity::cloud_enrollment::EnrollmentRequest,
    ) -> Result<()> {
        if reply.version != 1
            || reply.device_cert != self.current.material.value.certificate
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
pub(super) enum HostedStep {
    Inspect,
    Apply,
    Enrolled,
}
/// Scope of a Store that accepted this device's verified enrollment reply.
pub(super) struct EnrolledScope {
    pub room: String,
    pub workspace: u64,
    pub host: identity::DeviceCert,
    pub certificate: identity::DeviceCert,
    _credit: automerge::resource_limits::NativeReservation,
}

// Browser owner continuation after the verified Store reply. The host protocol
// test drives only the checkpointed first membership exchange above.
#[allow(
    dead_code,
    reason = "owner continuation is exercised by the browser build only"
)]
impl HostedAttempt {
    /// Fresh custody for an unbound Store: root, device and attempt keys are
    /// generated here and never leave serialized custody.
    pub(super) fn create_material(
        selected: &Selection,
        label: &str,
        now: u64,
        budget: &NativeAllowance,
    ) -> Result<Vec<u8>> {
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
        material.validate(now, budget)?;
        let bytes = material::encode_json(&material, MAX_MATERIAL, budget)?;
        // Round-trip through the one restore reader before custody is offered.
        let restored = Self::restore(
            &bytes.value,
            Selection {
                account_id: selected.account_id.clone(),
                machine_id: selected.machine_id.clone(),
            },
            budget,
            now,
        )?;
        drop(restored);
        let mut output = Vec::new();
        output
            .try_reserve_exact(bytes.value.len())
            .map_err(|_| HostedAttemptError::Capacity)?;
        output.extend_from_slice(&bytes.value);
        Ok(output)
    }
    /// Store.apply was refused for this signed request (lost first-claim
    /// race, expired or replaced challenge, certificate not yet valid). Stage
    /// a return to Inspect with the same keys and a fresh apply operation
    /// key. Like every checkpoint it moves custody only after its exact ACK.
    pub(super) fn reset_apply(&mut self, now: u64) -> Result<Checkpoint<'_>> {
        self.live()?;
        if !matches!(self.current.material.value.phase, Phase::Apply { .. }) {
            return Err(HostedAttemptError::Phase);
        }
        if let Some(staged) = &self.staged {
            if matches!(staged.material.value.phase, Phase::Initial) {
                return Ok(self.checkpoint());
            }
            return Err(HostedAttemptError::Checkpoint);
        }
        let retry = self
            .current
            .material
            .value
            .apply_retry
            .checked_add(1)
            .filter(|retry| *retry <= MAX_APPLY_RETRIES)
            .ok_or(HostedAttemptError::Capacity)?;
        let view = self
            .current
            .material
            .value
            .with_phase_retry(PhaseView::Initial, retry);
        let staged = self.stage_view(view, now)?;
        self.staged = Some(staged);
        Ok(self.checkpoint())
    }
    pub(super) fn step(&self) -> Result<HostedStep> {
        self.ready()?;
        Ok(match &self.current.material.value.phase {
            Phase::Initial => HostedStep::Inspect,
            Phase::Apply { .. } => HostedStep::Apply,
            Phase::MembershipChallenge { .. } | Phase::MembershipComplete { .. } => {
                HostedStep::Enrolled
            }
        })
    }
    pub(super) fn certificate(&self) -> &identity::DeviceCert {
        &self.current.material.value.certificate
    }
    pub(super) fn attempt_binding(&self) -> ([u8; 32], u64) {
        (
            self.current.material.value.attempt_id,
            self.current.material.value.created_at_ns,
        )
    }
    pub(super) fn current_material(&self) -> &[u8] {
        &self.current.bytes.value
    }
    fn enrolled_enrollment(&self) -> Result<Held<identity::cloud_enrollment::EnrollmentRequest>> {
        if self.step()? != HostedStep::Enrolled {
            return Err(HostedAttemptError::Phase);
        }
        self.enrollment()
    }
    pub(super) fn enrolled(&self) -> Result<EnrolledScope> {
        let enrollment = self.enrolled_enrollment()?;
        let certificate = &self.current.material.value.certificate;
        let credit = allocation::reserve(
            &self.budget,
            allocation::sum(&[
                enrollment.value.challenge.room.len(),
                enrollment.value.host_certificate.device_label.len(),
                certificate.device_label.len(),
            ])?,
        )?;
        Ok(EnrolledScope {
            room: enrollment.value.challenge.room.clone(),
            workspace: enrollment
                .value
                .challenge
                .workspace_id
                .parse()
                .map_err(|_| HostedAttemptError::Scope)?,
            host: enrollment.value.host_certificate.clone(),
            certificate: certificate.clone(),
            _credit: credit,
        })
    }
    /// The enrolled device key signs only closed onboarding wrappers/proofs.
    pub(super) fn device_key(&self) -> Result<SigningKey> {
        self.enrolled_enrollment()?;
        Ok(self.current.material.value.device())
    }
    /// A membership refresh uses a fresh request identifier; it never reuses
    /// the checkpointed first exchange whose challenge may have expired.
    pub(super) fn refresh_challenge(&self, request_id: [u8; 32]) -> Result<Request> {
        if request_id == [0; 32] {
            return Err(HostedAttemptError::Scope);
        }
        let enrollment = self.enrolled_enrollment()?;
        let certificate = &self.current.material.value.certificate;
        let _request = allocation::reserve(
            &self.budget,
            allocation::sum(&[
                enrollment.value.challenge.room.len(),
                certificate.device_label.len(),
            ])?,
        )?;
        Ok(Request::MembershipChallenge(
            join::MembershipChallengeRequest {
                room: enrollment.value.challenge.room.clone(),
                workspace_id: enrollment
                    .value
                    .challenge
                    .workspace_id
                    .parse()
                    .map_err(|_| HostedAttemptError::Scope)?,
                device_cert: certificate.clone(),
                request_id,
            },
        ))
    }
    pub(super) fn refresh_proof(
        &self,
        challenge: &join::MembershipChallenge,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<Request> {
        let enrollment = self.enrolled_enrollment()?;
        let candidate = &self.current.material.value;
        candidate.validate(now, &self.budget)?;
        candidate.check_membership_for(
            challenge,
            &enrollment.value.host_certificate,
            &enrollment.value.challenge,
            request_id,
            now,
            &self.budget,
        )?;
        let _errors = allocation::helper_errors(&self.budget)?;
        let _proof = allocation::reserve(&self.budget, challenge.room.len())?;
        let _transcript = allocation::transcript(challenge, &self.budget)?;
        let proof = join::sign_membership_proof(challenge.clone(), &candidate.device())
            .map_err(|_| HostedAttemptError::Signature)?;
        Ok(Request::MembershipComplete(
            join::MembershipCompleteRequest { proof },
        ))
    }
    /// Scope of an owner refresh reply. Authority is still established only by
    /// admitting its signed governance and deriving the structural Owner.
    pub(super) fn check_owner_bootstrap(
        &self,
        reply: &join::OwnerBootstrapV1,
        now: u64,
    ) -> Result<()> {
        let enrollment = self.enrolled_enrollment()?;
        self.current.material.value.validate(now, &self.budget)?;
        self.owner_reply_scope(reply, &enrollment.value)
    }
}

fn bounded_reply(
    bytes: &[u8],
    budget: &NativeAllowance,
) -> Result<qos_peer_state::resources::BoundedJoinResponse> {
    use relay::delivery_v3::onboarding_wire::MAX_RESPONSE_BYTES;
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_BYTES {
        return Err(HostedAttemptError::Response);
    }
    qos_peer_state::resources::BoundedJoinResponse::decode(
        bytes,
        qos_peer_state::resources::OuterLimits {
            allocation_bytes: 32 * 1024 * 1024,
            collection_entries: MAX_RESPONSE_BYTES,
        },
        budget,
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
    // These legacy routes are inert and are never passed to a transport.
    #[serde(rename = "action_address")]
    _action: serde::de::IgnoredAny,
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
        budget: &NativeAllowance,
    ) -> Result<()> {
        let _errors = allocation::helper_errors(budget)?;
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
