//! Challenged native person custody; these bytes grant no source or app access.
//! The root actor supplies its independently held installed certificate and
//! revocations. The wire cannot select an anchor or supply source credentials.
use serde::{Deserialize, Serialize};
use std::io;

pub const MAX_PROOF_BYTES: usize = 256 * 1024;
/// Canonical personal workspace spelling; this is vocabulary, not admission.
#[cfg(feature = "client")]
pub fn personal_room(person: &[u8; 32]) -> String {
    collab_types::person_room(person).to_string()
}
const DOMAIN: &[u8] = b"tilo-native-retained-person-proof-v1\0";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedLaunchBinding {
    pub operation_id: String,
    pub app_id: String,
    pub package_sha256: String,
    pub room: String,
    pub resource_record: String,
    pub target_uid: u32,
    /// The actual native Shell task serving the proof, not a wire PID claim.
    pub target_pid: u32,
    pub root_pid: u32,
    pub context_sha256: String,
}
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "retained native person proof refused",
    )
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}
impl RetainedLaunchBinding {
    /// # Errors
    /// Refuses malformed or unbounded frozen launch context.
    pub fn validate(&self) -> io::Result<()> {
        if !uuid(&self.operation_id)
            || !uuid(&self.room)
            || !digest(&self.package_sha256)
            || !digest(&self.context_sha256)
            || self.target_uid == 0
            || self.target_pid == 0
            || self.root_pid == 0
            || self.target_pid == self.root_pid
            || [&self.app_id, &self.resource_record]
                .into_iter()
                .any(|value| {
                    value.is_empty() || value.len() > 512 || value.chars().any(char::is_control)
                })
        {
            return Err(refused());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedPersonClaims {
    pub binding: RetainedLaunchBinding,
    pub challenge: [u8; 32],
    pub certificate: Vec<u8>,
    pub revocations: Vec<tilo_user_identity::RevocationEntry>,
    pub execution_id: String,
    pub person_room: String,
}
impl RetainedPersonClaims {
    /// # Errors
    /// Refuses malformed claims or a message exceeding the fixed bound.
    pub fn signing_bytes(&self) -> io::Result<Vec<u8>> {
        self.binding.validate()?;
        if !uuid(&self.execution_id)
            || !uuid(&self.person_room)
            || self.challenge == [0; 32]
            || self.certificate.len() > 16 * 1024
            || self.certificate.is_empty()
            || self.revocations.len() > 256
        {
            return Err(refused());
        }
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(serde_json::to_vec(self).map_err(io::Error::other)?);
        if bytes.len() > MAX_PROOF_BYTES {
            return Err(refused());
        }
        Ok(bytes)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRetainedPersonProof {
    pub claims: RetainedPersonClaims,
    pub signature: Vec<u8>,
}
/// Root-local custody created only by an actually verified wire reply. This
/// type never crosses the transport; later replies must preserve this epoch.
#[cfg(all(target_os = "linux", feature = "client"))]
pub struct VerifiedRetainedPersonEpoch {
    binding: RetainedLaunchBinding,
    execution_id: String,
    certificate: Vec<u8>,
    revoked: std::sync::atomic::AtomicBool,
}
#[cfg(all(target_os = "linux", feature = "client"))]
impl VerifiedRetainedPersonEpoch {
    /// # Errors
    /// Refuses a retired epoch, changed execution/certificate, or a fresh reply
    /// failing actual kernel/installed custody and signature verification.
    /// Every failure permanently retires this epoch.
    pub fn revalidate(
        &self,
        proof: &SignedRetainedPersonProof,
        challenge: &[u8; 32],
        shell: &runtime_portal::process_binding::ExecutingPeer,
        installed: &tilo_user_identity::DeviceCert,
        revocations: &tilo_user_identity::RevocationList,
    ) -> io::Result<()> {
        use std::sync::atomic::Ordering;
        let checked = (|| {
            if self.revoked.load(Ordering::Acquire)
                || proof.claims.execution_id != self.execution_id
                || proof.claims.certificate != self.certificate
            {
                return Err(refused());
            }
            proof.verify(&self.binding, challenge, shell, installed, revocations)?;
            if self.revoked.load(Ordering::Acquire) {
                return Err(refused());
            }
            Ok(())
        })();
        if checked.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        checked
    }
    /// # Errors
    /// Refresh a daemon verification of the SAME genuinely executing retained
    /// helper recipient. Failure latches exactly like ordinary epoch refresh.
    pub fn revalidate_for_retained_helper(
        &self,
        proof: &SignedRetainedPersonProof,
        challenge: &[u8; 32],
        shell: &runtime_portal::process_binding::ExecutingPeer,
        installed: &tilo_user_identity::DeviceCert,
        revocations: &tilo_user_identity::RevocationList,
        helper: &runtime_portal::retained_role::VerifiedHelperPeer,
    ) -> io::Result<()> {
        use std::sync::atomic::Ordering;
        let checked = (|| {
            if self.revoked.load(Ordering::Acquire)
                || proof.claims.execution_id != self.execution_id
                || proof.claims.certificate != self.certificate
            {
                return Err(refused());
            }
            proof.verify_for_retained_helper(
                &self.binding,
                challenge,
                shell,
                installed,
                revocations,
                helper,
            )?;
            if self.revoked.load(Ordering::Acquire) {
                return Err(refused());
            }
            Ok(())
        })();
        if checked.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        checked
    }
    pub fn revoke(&self) {
        self.revoked
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
impl SignedRetainedPersonProof {
    /// # Errors
    /// Refuses malformed claims, signature length or oversized wire bytes.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.claims.signing_bytes()?;
        if self.signature.len() != 64 {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > MAX_PROOF_BYTES {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Refuses oversized, unknown-field or malformed proof messages.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_PROOF_BYTES {
            return Err(refused());
        }
        let proof: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        proof.encode()?;
        Ok(proof)
    }
    /// Verify one fresh reply against ROOT-LOADED installed custody and an
    /// independently QPE1-pinned Shell task. A verified reply is not a launch
    /// permit: retain the live channel and request a fresh proof at each actual
    /// source/launch boundary. The root actor still checks A and accepted policy.
    /// # Errors
    /// Refuses stale kernel custody, replay, changed context, untrusted/expired/
    /// revoked certificate, or an invalid native-device signature.
    #[cfg(all(target_os = "linux", feature = "client"))]
    pub fn verify(
        &self,
        expected: &RetainedLaunchBinding,
        challenge: &[u8; 32],
        shell: &runtime_portal::process_binding::ExecutingPeer,
        installed: &tilo_user_identity::DeviceCert,
        revocations: &tilo_user_identity::RevocationList,
    ) -> io::Result<VerifiedRetainedPersonEpoch> {
        let entries =
            self.authenticated_revocations(expected, challenge, shell, installed, revocations)?;
        let mut merged = revocations.clone();
        for entry in entries {
            merged.insert(entry).map_err(io::Error::other)?;
        }
        if merged.is_revoked(&installed.device_public_key) {
            return Err(refused());
        }
        Ok(VerifiedRetainedPersonEpoch {
            binding: expected.clone(),
            execution_id: self.claims.execution_id.clone(),
            certificate: self.claims.certificate.clone(),
            revoked: std::sync::atomic::AtomicBool::new(false),
        })
    }
    /// # Errors
    /// The recipient is an opaque actual installed helper role, never a root
    /// PID/UID/program label supplied by a wire request. This does not grant
    /// source access or replace daemon Author/installed enrollment checks.
    #[cfg(all(target_os = "linux", feature = "client"))]
    pub fn verify_for_retained_helper(
        &self,
        expected: &RetainedLaunchBinding,
        challenge: &[u8; 32],
        shell: &runtime_portal::process_binding::ExecutingPeer,
        installed: &tilo_user_identity::DeviceCert,
        revocations: &tilo_user_identity::RevocationList,
        helper: &runtime_portal::retained_role::VerifiedHelperPeer,
    ) -> io::Result<VerifiedRetainedPersonEpoch> {
        helper.check_current()?;
        let entries = self.authenticated_revocations_for_recipient(
            expected,
            challenge,
            shell,
            installed,
            revocations,
            helper.peer().pid(),
        )?;
        let mut merged = revocations.clone();
        for entry in entries {
            merged.insert(entry).map_err(io::Error::other)?;
        }
        if merged.is_revoked(&installed.device_public_key) {
            return Err(refused());
        }
        helper.check_current()?;
        Ok(VerifiedRetainedPersonEpoch {
            binding: expected.clone(),
            execution_id: self.claims.execution_id.clone(),
            certificate: self.claims.certificate.clone(),
            revoked: std::sync::atomic::AtomicBool::new(false),
        })
    }
    /// Authenticate revocations for a daemon receiving a genuinely delegated
    /// helper-bound proof. The actual installed helper role remains the signed
    /// binding's recipient; no serialized PID can substitute for its custody.
    /// # Errors
    /// Refuses a retired helper or invalid fresh proof/context/certificate.
    #[cfg(all(target_os = "linux", feature = "client"))]
    pub fn authenticated_revocations_for_retained_helper(
        &self,
        expected: &RetainedLaunchBinding,
        challenge: &[u8; 32],
        shell: &runtime_portal::process_binding::ExecutingPeer,
        installed: &tilo_user_identity::DeviceCert,
        revocations: &tilo_user_identity::RevocationList,
        helper: &runtime_portal::retained_role::VerifiedHelperPeer,
    ) -> io::Result<Vec<tilo_user_identity::RevocationEntry>> {
        helper.check_current()?;
        let entries = self.authenticated_revocations_for_recipient(
            expected,
            challenge,
            shell,
            installed,
            revocations,
            helper.peer().pid(),
        )?;
        helper.check_current()?;
        Ok(entries)
    }
    /// Authenticate the complete fresh proof BEFORE returning root-signed
    /// revocations for monotonic root publication. This verifies issuer/root,
    /// entry signatures and exact installed certificate/device signature; wire
    /// claims alone can never update root custody. Afterwards the actor must
    /// merge entries monotonically, then run final verify/epoch.revalidate using
    /// the refreshed root list. An embedded revocation of this signer prevents
    /// final admission even though its valid root statement can be retained.
    /// # Errors
    /// Refuses invalid kernel/context/challenge/certificate/proof/entry custody.
    #[cfg(all(target_os = "linux", feature = "client"))]
    pub fn authenticated_revocations(
        &self,
        expected: &RetainedLaunchBinding,
        challenge: &[u8; 32],
        shell: &runtime_portal::process_binding::ExecutingPeer,
        installed: &tilo_user_identity::DeviceCert,
        revocations: &tilo_user_identity::RevocationList,
    ) -> io::Result<Vec<tilo_user_identity::RevocationEntry>> {
        self.authenticated_revocations_for_recipient(
            expected,
            challenge,
            shell,
            installed,
            revocations,
            std::process::id(),
        )
    }
    #[cfg(all(target_os = "linux", feature = "client"))]
    fn authenticated_revocations_for_recipient(
        &self,
        expected: &RetainedLaunchBinding,
        challenge: &[u8; 32],
        shell: &runtime_portal::process_binding::ExecutingPeer,
        installed: &tilo_user_identity::DeviceCert,
        revocations: &tilo_user_identity::RevocationList,
        recipient_pid: u32,
    ) -> io::Result<Vec<tilo_user_identity::RevocationEntry>> {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        expected.validate()?;
        if effective_uid() != 0
            || expected.root_pid != recipient_pid
            || !shell.current()
            || shell.uid() != expected.target_uid
            || shell.pid() != expected.target_pid
            || &self.claims.binding != expected
            || &self.claims.challenge != challenge
        {
            return Err(refused());
        }
        let cert: tilo_user_identity::DeviceCert =
            serde_json::from_slice(&self.claims.certificate).map_err(io::Error::other)?;
        // Root's kernel wall clock, never a wire timestamp or environment
        // override. The canonical verifier checks signature, interval,
        // not-before and expiry for BOTH independent and received custody.
        let now_ns = current_time_ns()?;
        tilo_user_identity::verify_device_cert_at(installed, now_ns).map_err(io::Error::other)?;
        tilo_user_identity::verify_device_cert_at(&cert, now_ns).map_err(io::Error::other)?;
        if self.claims.certificate != serde_json::to_vec(installed).map_err(io::Error::other)?
            || cert.capabilities.foreign
            || revocations.is_revoked(&cert.device_public_key)
            || self.claims.binding.room != self.claims.person_room
            || self.claims.person_room != personal_room(&cert.user_public_key)
        {
            return Err(refused());
        }
        let signature: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| refused())?;
        VerifyingKey::from_bytes(&cert.device_public_key)
            .map_err(io::Error::other)?
            .verify(
                &self.claims.signing_bytes()?,
                &Signature::from_bytes(&signature),
            )
            .map_err(io::Error::other)?;
        for entry in self.claims.revocations.iter().chain(revocations.iter()) {
            if entry.user_public_key != installed.user_public_key {
                return Err(refused());
            }
            tilo_user_identity::verify_revocation_entry(entry).map_err(io::Error::other)?;
        }
        if !shell.current() {
            return Err(refused());
        }
        // The bounded entry checks can cross an expiry boundary. Recheck at
        // the actual publication boundary before returning authenticated data.
        let now_ns = current_time_ns()?;
        tilo_user_identity::verify_device_cert_at(installed, now_ns).map_err(io::Error::other)?;
        tilo_user_identity::verify_device_cert_at(&cert, now_ns).map_err(io::Error::other)?;
        Ok(self.claims.revocations.clone())
    }
}

// Kernel identity is observed independently of deserialized proof claims.
#[cfg(all(target_os = "linux", feature = "client"))]
fn effective_uid() -> libc::uid_t {
    rustix::process::geteuid().as_raw()
}

#[cfg(all(test, target_os = "linux", feature = "native-retained-qualification"))]
mod tests;

#[cfg(all(target_os = "linux", feature = "client"))]
fn current_time_ns() -> io::Result<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos(),
    )
    .map_err(io::Error::other)
}
