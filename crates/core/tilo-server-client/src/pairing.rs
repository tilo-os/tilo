//! What a person's root takes back from their devices: the revocation every
//! server and device refuses a key with.

use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry_async};
#[cfg(test)]
use tilo_user_identity::{RevocationList, issue_device_cert_async};

use crate::error::CertifyError;
use crate::signer::DeviceSigner;

/// Have `root` revoke `device_public_key` as of `at_ns`, for `reason`: the
/// entry every server and device refuses that key from then on with, while what
/// it signed before stays valid.
///
/// # Errors
///
/// [`CertifyError::Request`] for the root's own key, when signing fails, or
/// when the entry does not verify.
pub async fn revoke_device(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    at_ns: u64,
    reason: RevocationReason,
) -> Result<RevocationEntry, CertifyError> {
    let request = |error: &dyn std::fmt::Display| CertifyError::Request(error.to_string());
    let user_public_key = root.device_public_key();
    if device_public_key == user_public_key {
        return Err(CertifyError::Request(
            "a root revokes the keys it made, never itself".into(),
        ));
    }
    let mut entry = RevocationEntry {
        user_public_key,
        revoked_device_public_key: device_public_key,
        revoked_at_ns: at_ns,
        reason,
        signature: [0; 64],
    };
    sign_revocation_entry_async(&mut entry, root)
        .await
        .map_err(|e| request(&e))?;
    tilo_user_identity::verify_revocation_entry(&entry).map_err(|e| request(&e))?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        CERTIFICATION_SCHEMA_VERSION, CertificationPolicy, CertificationRequest,
        DeviceCapabilities, verify_device_cert,
    };

    #[tokio::test]
    async fn a_page_root_signs_the_same_certificate_and_revocation_as_a_held_key() {
        // A page's root answers only later; a held root answers at once. The
        // bytes either signs are the same, so a certificate a browser's root
        // made and one a machine's root made verify alike.
        let root = SigningKey::from_bytes(&[11; 32]);
        let device = SigningKey::from_bytes(&[12; 32]).verifying_key().to_bytes();
        let now = 1_700_000_000_000_000_000;
        let request = CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "Browser".into(),
            requested: DeviceCapabilities::member(),
            nonce: [9; 16],
            invite: [0; 16],
            issued_at_ns: now,
            expires_at_ns: now + 60_000_000_000,
            signature: [0; 64],
        }
        .sign(&SigningKey::from_bytes(&[12; 32]))
        .unwrap();
        let policy = CertificationPolicy::browser();
        let awaited =
            issue_device_cert_async(&request, &root, &policy, &RevocationList::new(), now)
                .await
                .unwrap();
        let at_once = tilo_user_identity::issue_device_cert(
            &request,
            &root,
            &policy,
            &RevocationList::new(),
            now,
        )
        .unwrap();
        assert_eq!(awaited, at_once);
        assert_eq!(verify_device_cert(&awaited), Ok(()));

        let mut later = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: now,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_entry_async(&mut later, &root)
            .await
            .unwrap();
        let mut held = later.clone();
        held.signature = [0; 64];
        tilo_user_identity::sign_revocation_entry(&mut held, &root).unwrap();
        assert_eq!(later, held);
        assert_eq!(tilo_user_identity::verify_revocation_entry(&later), Ok(()));
    }
}
