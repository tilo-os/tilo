//! Signing a person in again through their agent: a device of the person
//! claims a sign-in code ([`tilo_relay_protocol::login_code::Claim`]), and
//! the person's stand-in approves the one certification request that names
//! the invite the claim minted.

use std::sync::Arc;

use hyper::{Response, StatusCode};
use tilo_relay_protocol::certify::MEMBER_APPROVAL_SKEW_NS;
use tilo_relay_protocol::login_code::{Claim, Claimed, claim_message};
use tilo_user_identity::{CertificationRequest, DeviceCert, RevocationList};

use super::{Body, Web, code_refused, json, login_codes::LoginCodes, refused, stand_in::StandIns};
use crate::certification::StandInApprover;

/// Approves, for a person's stand-in, the requests their claims expect.
struct Approver {
    codes: Arc<LoginCodes>,
    stand_ins: Arc<StandIns>,
}

const fn ms(now_ns: u64) -> u64 {
    now_ns / 1_000_000
}

impl StandInApprover for Approver {
    fn expects(&self, request: &CertificationRequest, now_ns: u64) -> bool {
        self.codes
            .claimed(&request.user_public_key, &request.invite, ms(now_ns))
            && self
                .stand_ins
                .live(&request.user_public_key, &RevocationList::new(), now_ns)
                .is_some()
    }

    fn issue(
        &self,
        request: &CertificationRequest,
        revocations: &RevocationList,
        now_ns: u64,
    ) -> Option<DeviceCert> {
        let (key, stand_in) = self
            .stand_ins
            .live(&request.user_public_key, revocations, now_ns)?;
        if !self
            .codes
            .take_claim(&request.user_public_key, &request.invite, ms(now_ns))
        {
            return None;
        }
        tilo_user_identity::issue_device_cert_as_stand_in(
            request,
            &key,
            &stand_in,
            revocations,
            now_ns,
        )
        .ok()
    }

    fn approved(&self, request: &CertificationRequest) {
        self.codes
            .approved(&request.user_public_key, &request.invite);
    }
}

impl Web {
    /// Let people's stand-ins here approve the requests their claims expect.
    pub(super) fn serve_stand_ins(&self) {
        self.context.certification.set_stand_in(Arc::new(Approver {
            codes: Arc::clone(&self.login_codes),
            stand_ins: Arc::clone(&self.stand_ins),
        }));
    }

    /// A device of a person claims a sign-in code for them.
    #[allow(
        clippy::result_large_err,
        reason = "the error is the response the handler sends as it is"
    )]
    pub(super) fn claim(&self, claim: &Claim) -> Result<Response<Body>, Response<Body>> {
        let now_ns = tilo_clock::unix_now_ns();
        let millis = ms(now_ns);
        let forbidden = |message: &str| refused(StatusCode::FORBIDDEN, message);
        self.login_codes
            .claimable(&claim.report, millis)
            .map_err(|refusal| code_refused(&refusal))?;
        let cert: DeviceCert = hex::decode(&claim.cert)
            .ok()
            .and_then(|bytes| postcard::from_bytes(&bytes).ok())
            .ok_or_else(|| refused(StatusCode::BAD_REQUEST, "a certificate is postcard as hex"))?;
        let mut signature = [0; 64];
        hex::decode_to_slice(&claim.signature, &mut signature)
            .map_err(|_| refused(StatusCode::BAD_REQUEST, "a signature is 64 bytes hex"))?;
        tilo_user_identity::verify_device_cert_at(&cert, now_ns)
            .map_err(|_| forbidden("this is no live certificate of a device"))?;
        if cert.capabilities.stand_in {
            return Err(forbidden("a stand-in is no device of the person"));
        }
        let revocations = self.context.certification.revocations();
        if revocations.refuses(&cert) {
            return Err(forbidden("this device is revoked"));
        }
        if claim.signed_at_ns.abs_diff(now_ns) > MEMBER_APPROVAL_SKEW_NS {
            return Err(forbidden("the claim is too old or from the future"));
        }
        ed25519_dalek::VerifyingKey::from_bytes(&cert.device_public_key)
            .and_then(|device| {
                device.verify_strict(
                    &claim_message(&claim.report, claim.signed_at_ns),
                    &ed25519_dalek::Signature::from_bytes(&signature),
                )
            })
            .map_err(|_| forbidden("the claim is not signed by this device"))?;
        let user = cert.user_public_key;
        if self.stand_ins.live(&user, &revocations, now_ns).is_none() {
            return Err(forbidden(
                "this person keeps no stand-in on this host to approve the tab",
            ));
        }
        let expires_at_ms = self
            .login_codes
            .claim(&claim.report, user, cert.device_label, millis)
            .map_err(|refusal| code_refused(&refusal))?;
        Ok(json(&Claimed { expires_at_ms }))
    }
}
