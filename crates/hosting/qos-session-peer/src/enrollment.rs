//! Serialized root-authorized issuance. HTTP authorization belongs to enrollment_http.
use crate::SessionOwner;
use qos_peer_state::enrollment::{
    self, ChallengeRequest, ChallengeResponse, CompleteRequest, EnrollmentChallenge,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, WorkspaceDirectory, sign_device_cert};
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Issuance {
    pub device_public_key: [u8; 32],
    pub request_id: [u8; 32],
    pub certificate: DeviceCert,
}
#[derive(Debug)]
pub struct EnrollmentError {
    pub status: u16,
    pub message: String,
}
impl std::fmt::Display for EnrollmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for EnrollmentError {}
impl EnrollmentError {
    pub(crate) fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    fn unavailable(error: impl std::fmt::Display) -> Self {
        Self::new(503, error.to_string())
    }
}
#[derive(Default)]
pub struct EnrollmentAuthority {
    challenges: BTreeMap<[u8; 32], EnrollmentChallenge>,
}
fn decode_key(value: &str) -> Result<[u8; 32], EnrollmentError> {
    if value.len() != 64 {
        return Err(EnrollmentError::new(
            400,
            "public key and request ID must be64 hexadecimal characters",
        ));
    }
    hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| EnrollmentError::new(400, "invalid hexadecimal enrollment identity"))
}
fn check_device(owner: &SessionOwner, device: &[u8; 32]) -> Result<(), EnrollmentError> {
    if owner
        .peer()
        .revocations()
        .iter()
        .any(|entry| entry.revoked_device_public_key == *device)
    {
        return Err(EnrollmentError::new(403, "device revoked"));
    }
    if !owner.permits_publication() {
        return Err(EnrollmentError::new(
            503,
            owner
                .peer()
                .incomplete_reason()
                .unwrap_or_else(|| "native durability unavailable; restore required".into()),
        ));
    }
    Ok(())
}
impl EnrollmentAuthority {
    pub fn challenge(
        &mut self,
        owner: &SessionOwner,
        request: ChallengeRequest,
        now: u64,
    ) -> Result<ChallengeResponse, EnrollmentError> {
        let device = decode_key(&request.device_public_key)?;
        let request_id = decode_key(&request.request_id)?;
        ed25519_dalek::VerifyingKey::from_bytes(&device)
            .map_err(|_| EnrollmentError::new(400, "invalid Ed25519 public key"))?;
        check_device(owner, &device)?;
        self.challenges.retain(|_, entry| entry.expires_at_ns > now);
        // An explicit short-lived challenge memory budget, independent of admitted viewers.
        let used = self
            .challenges
            .values()
            .map(|entry| {
                postcard::to_stdvec(entry)
                    .map(|bytes| bytes.len())
                    .unwrap_or(0)
            })
            .sum::<usize>();
        if used >= 1024 * 1024 {
            return Err(EnrollmentError::new(
                429,
                "pending enrollment challenges exceed local memory budget; retry after expiry",
            ));
        }
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let challenge = EnrollmentChallenge {
            version: 2,
            root_public_key: owner.peer().authority().user_public_key,
            room: owner.peer().room().into(),
            workspace_id: owner.workspace_id(),
            device_public_key: device,
            request_id,
            nonce,
            expires_at_ns: now.saturating_add(60_000_000_000),
        };
        let encoded =
            enrollment::encode(&challenge).map_err(|error| EnrollmentError::new(400, error))?;
        self.challenges.insert(nonce, challenge);
        Ok(ChallengeResponse {
            version: 2,
            challenge: encoded,
        })
    }
    /// Returns an internal committed certificate. The HTTP owner MUST reconcile relay
    /// membership before constructing a public bootstrap response.
    pub fn complete(
        &mut self,
        owner: &mut SessionOwner,
        request: CompleteRequest,
        now: u64,
    ) -> Result<DeviceCert, EnrollmentError> {
        let challenge = enrollment::verify(
            &request,
            owner.peer().authority().user_public_key,
            owner.peer().room(),
            owner.workspace_id(),
            now,
        )
        .map_err(|error| EnrollmentError::new(403, error))?;
        if self.challenges.get(&challenge.nonce) != Some(&challenge) {
            return Err(EnrollmentError::new(
                409,
                "enrollment challenge was consumed or belongs to an earlier host session; request a fresh challenge",
            ));
        }
        self.challenges.remove(&challenge.nonce);
        check_device(owner, &challenge.device_public_key)?;
        if let Some(prior) = owner.issuances().iter().find(|entry| {
            entry.device_public_key == challenge.device_public_key
                && entry.request_id == challenge.request_id
        }) {
            let certificate = prior.certificate.clone();
            owner.flush().map_err(EnrollmentError::unavailable)?;
            return Ok(certificate);
        }
        let existing = owner
            .issuances()
            .iter()
            .find(|entry| entry.device_public_key == challenge.device_public_key)
            .map(|entry| entry.certificate.clone());
        let reused_certificate = existing.is_some();
        let certificate = if let Some(certificate) = existing {
            certificate
        } else {
            if owner
                .peer()
                .certificates()
                .iter()
                .any(|cert| cert.device_public_key == challenge.device_public_key)
            {
                return Err(EnrollmentError::new(
                    403,
                    "identity is reserved by the native authority",
                ));
            }
            let mut cert = DeviceCert {
                user_public_key: owner.peer().authority().user_public_key,
                device_public_key: challenge.device_public_key,
                device_label: "Browser viewer".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: now,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &owner.identity.root)
                .map_err(EnrollmentError::unavailable)?;
            cert
        };
        let mut candidate = owner.peer().clone();
        let governance_changed = candidate
            .admit_certificate(
                certificate.clone(),
                &owner.identity.native,
                web_time::SystemTime::now(),
            )
            .map_err(EnrollmentError::unavailable)?;
        let mut directory = WorkspaceDirectory::load(
            certificate.user_public_key,
            owner.peer().authority().device_public_key,
            owner.directory(),
        )
        .map_err(EnrollmentError::unavailable)?;
        let workspace = challenge.workspace_id;
        let mut members = directory
            .as_map()
            .get(&workspace)
            .ok_or_else(|| EnrollmentError::new(503, "canonical directory workspace missing"))?
            .member_devices
            .clone();
        let directory_changed = !members.contains(&certificate.device_public_key);
        if directory_changed {
            members.push(certificate.device_public_key);
            members.sort();
            directory
                .set_member_devices(workspace, &members)
                .map_err(EnrollmentError::unavailable)?;
        }
        let issuance = Issuance {
            device_public_key: challenge.device_public_key,
            request_id: challenge.request_id,
            certificate: certificate.clone(),
        };
        if reused_certificate && !governance_changed && !directory_changed {
            owner.commit_existing_issuance(issuance)
        } else {
            owner.commit_enrollment(candidate, directory.save(), issuance)
        }
        .map_err(EnrollmentError::unavailable)?;
        Ok(certificate)
    }
}
/// Rust serializes timestamp-bearing fields directly into the final raw HTTP body.
pub fn public_bootstrap(
    owner: &SessionOwner,
    certificate: &DeviceCert,
    relay_address: serde_json::Value,
    packet_relay: &str,
    now: u64,
) -> Result<serde_json::Value, EnrollmentError> {
    check_device(owner, &certificate.device_public_key)?;
    if !owner
        .issuances()
        .iter()
        .any(|entry| &entry.certificate == certificate)
    {
        return Err(EnrollmentError::new(
            403,
            "certificate has no committed issuance",
        ));
    }
    let mut directory = WorkspaceDirectory::load(
        certificate.user_public_key,
        owner.peer().authority().device_public_key,
        owner.directory(),
    )
    .map_err(EnrollmentError::unavailable)?;
    let update = directory
        .signed_snapshot(&owner.identity.native, now)
        .map_err(EnrollmentError::unavailable)?;
    let revocations = owner.peer().revocations();
    // StatePeer::new admits an active roster. Retired certificates remain in
    // durable history, but must not prevent an unrelated viewer from starting.
    let roster = owner
        .peer()
        .certificates()
        .into_iter()
        .filter(|cert| {
            !revocations
                .iter()
                .any(|entry| entry.revoked_device_public_key == cert.device_public_key)
        })
        .collect::<Vec<_>>();
    Ok(
        serde_json::json!({"version":2,"certificate":certificate,"root_public_key":certificate.user_public_key,"host_certificate":owner.host_certificate(),"relay_address":relay_address,"packet_relay":packet_relay,"revocations":revocations,"bootstrap":{"directory_update":update,"directory_author":owner.peer().authority(),"certified_roster":roster,"room":owner.peer().room(),"workspace_id":owner.workspace_id().to_string(),"writer_tab_id":1,"chat_package_primary":true,"records":owner.peer().rows()}}),
    )
}
