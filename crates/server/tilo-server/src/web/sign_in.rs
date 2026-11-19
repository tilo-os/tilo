//! Signing a person in again through their agent, the one way: a device of
//! the person that may admit devices claims a sign-in code
//! ([`tilo_server_protocol::login_code::Claim`]), reads back the one
//! certification request that names the invite the claim minted, and
//! answers it with a certificate it signs, as any approver answers a
//! pairing.

use std::sync::Arc;

use hyper::{Response, StatusCode};
use tilo_server_protocol::certify::MEMBER_APPROVAL_SKEW_NS;
use tilo_server_protocol::login_code::{
    Certify, Claim, ClaimRefused, Claimed, Waiting, WaitingRequest, claim_digest,
};
use tilo_user_identity::{CertificationRequest, DeviceCert};

use super::login_codes::{LoginCodes, Refusal};
use super::{Body, Web, code_refused, json, json_status, refused};
use crate::certification::CodeRequests;

/// The requests this server's login codes expect.
struct Expected(Arc<LoginCodes>);

const fn ms(now_ns: u64) -> u64 {
    now_ns / 1_000_000
}

impl CodeRequests for Expected {
    fn sets_up(&self, request: &CertificationRequest, now_ns: u64) -> bool {
        self.0
            .sets_up(&request.user_public_key, &request.invite, ms(now_ns))
    }

    fn expects(&self, request: &CertificationRequest, now_ns: u64) -> bool {
        self.0
            .claimed(&request.user_public_key, &request.invite, ms(now_ns))
    }
}

/// A claim refused for `reason`: its sentence, and the reason by name.
fn claim_refused(reason: ClaimRefused) -> Response<Body> {
    json_status(
        StatusCode::FORBIDDEN,
        &serde_json::json!({ "error": reason.sentence(), "reason": reason }),
    )
}

/// `text` as the postcard-as-hex certificate it carries.
#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn certificate(text: &str) -> Result<DeviceCert, Response<Body>> {
    hex::decode(text)
        .ok()
        .and_then(|bytes| postcard::from_bytes(&bytes).ok())
        .ok_or_else(|| refused(StatusCode::BAD_REQUEST, "a certificate is postcard as hex"))
}

impl Web {
    /// Queue the requests this server's login codes expect.
    pub(super) fn serve_code_requests(&self) {
        self.context
            .certification
            .set_code_requests(Arc::new(Expected(Arc::clone(&self.login_codes))));
    }

    /// A device of a person that may admit devices claims a sign-in code
    /// for them.
    #[allow(
        clippy::result_large_err,
        reason = "the error is the response the handler sends as it is"
    )]
    pub(super) fn claim(&self, claim: &Claim) -> Result<Response<Body>, Response<Body>> {
        let now_ns = tilo_clock::unix_now_ns();
        let millis = ms(now_ns);
        self.login_codes
            .claimable(&claim.report, millis)
            .map_err(|refusal| code_refused(&refusal))?;
        let cert = certificate(&claim.cert)?;
        let mut signature = [0; 64];
        hex::decode_to_slice(&claim.signature, &mut signature)
            .map_err(|_| refused(StatusCode::BAD_REQUEST, "a signature is 64 bytes hex"))?;
        tilo_user_identity::verify_device_cert_at(&cert, now_ns)
            .map_err(|_| claim_refused(ClaimRefused::NotLive))?;
        if self.context.certification.revocations().refuses(&cert) {
            return Err(claim_refused(ClaimRefused::Revoked));
        }
        if claim.signed_at_ns.abs_diff(now_ns) > MEMBER_APPROVAL_SKEW_NS {
            return Err(claim_refused(ClaimRefused::Clock));
        }
        let digest = claim_digest(&claim.report, claim.signed_at_ns)
            .map_err(|_| refused(StatusCode::BAD_REQUEST, "the claim cannot be encoded"))?;
        ed25519_dalek::VerifyingKey::from_bytes(&cert.device_public_key)
            .and_then(|device| {
                device.verify_strict(&digest, &ed25519_dalek::Signature::from_bytes(&signature))
            })
            .map_err(|_| claim_refused(ClaimRefused::Signature))?;
        // The device that claims is the one that certifies the tab.
        if !cert.capabilities.can_admit_devices {
            return Err(claim_refused(ClaimRefused::CannotAdmit));
        }
        let expires_at_ms = self
            .login_codes
            .claim(
                &claim.report,
                cert.user_public_key,
                cert.device_label,
                millis,
            )
            .map_err(|refusal| code_refused(&refusal))?;
        Ok(json(&Claimed { expires_at_ms }))
    }

    /// The tab's request, for the device that claimed its code to certify.
    pub(super) fn waiting(&self, waiting: &Waiting) -> Result<WaitingRequest, Refusal> {
        let now_ns = tilo_clock::unix_now_ns();
        let claim = self.login_codes.claim_of(&waiting.report, ms(now_ns))?;
        let request = self
            .context
            .certification
            .waiting_for(&claim.user, &claim.invite, now_ns)
            .and_then(|request| postcard::to_allocvec(&request).ok())
            .map(hex::encode);
        Ok(WaitingRequest { request })
    }

    /// The device that claimed a code answers its tab's request.
    #[allow(
        clippy::result_large_err,
        reason = "the error is the response the handler sends as it is"
    )]
    pub(super) fn certify(&self, certify: &Certify) -> Result<(), Response<Body>> {
        let now_ns = tilo_clock::unix_now_ns();
        let claim = self
            .login_codes
            .claim_of(&certify.report, ms(now_ns))
            .map_err(|refusal| code_refused(&refusal))?;
        let cert = certificate(&certify.cert)?;
        let codes = &self.login_codes;
        self.context
            .certification
            .issue_named(
                &claim.user,
                &claim.invite,
                cert,
                |request| {
                    let taken =
                        codes.take_claim(&request.user_public_key, &request.invite, ms(now_ns));
                    // Recorded before the tab can read its answer, so a tab
                    // that follows its code after being certified sees it.
                    if taken {
                        codes.approved(&request.user_public_key, &request.invite);
                    }
                    taken
                },
                now_ns,
            )
            .map_err(|reason| refused(StatusCode::FORBIDDEN, &reason))?;
        Ok(())
    }
}
