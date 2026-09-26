//! Identity-rooted endorsements for exact OCI operating-system images.
//!
//! The human root certifies a primary device with [`crate::DeviceCert`]; that
//! device signs an immutable OCI manifest digest and generation here. Verifiers
//! therefore get one explicit chain: human identity → authorized device → image.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    DeviceCert, DeviceCertError, RevocationList,
    canonical::{CanonicalError, signing_digest},
    verify_device_cert_at,
};

/// Frozen signing domain for [`ImageEndorsement`].
pub const IMAGE_ENDORSEMENT_PREIMAGE_TAG: &[u8] = b"tilo-image-endorsement-v1\0";
/// Frozen signing domain for [`ImageSigningAuthorization`].
pub const IMAGE_SIGNING_AUTHORIZATION_PREIMAGE_TAG: &[u8] =
    b"tilo-image-signing-authorization-v1\0";
/// Maximum lifetime of a device's image-signing grant (30 days).
///
/// A finite `u64` expiry is not a useful exposure bound if it can be centuries
/// away. Human roots renew this narrow authority; device membership itself is
/// a separate certificate and is unaffected by this limit.
pub const MAX_IMAGE_SIGNING_AUTHORIZATION_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;
/// Maximum age of an image endorsement accepted by an updating machine.
///
/// A distinct, shorter bound prevents a frozen registry from replaying one
/// release for the entire lifetime of the device's signing authorization.
pub const MAX_IMAGE_ENDORSEMENT_AGE_NS: u64 = 7 * 24 * 60 * 60 * 1_000_000_000;

/// Human-root authorization for one device key to endorse OS images.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSigningAuthorization {
    /// Human identity root granting this authority.
    pub user_public_key: [u8; 32],
    /// Exact device key allowed to endorse images.
    pub device_public_key: [u8; 32],
    /// Time at which the human root granted the authority.
    pub issued_at_ns: u64,
    /// Mandatory expiry limits exposure of an unattended build signer.
    pub expires_at_ns: u64,
    /// Human-root Ed25519 signature over the domain-separated canonical view.
    #[serde(with = "crate::cert::serde_bytes_64")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct ImageSigningAuthorizationView<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    issued_at_ns: u64,
    expires_at_ns: u64,
}

impl ImageSigningAuthorization {
    fn view(&self) -> ImageSigningAuthorizationView<'_> {
        ImageSigningAuthorizationView {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        }
    }
}

/// A primary device's signed endorsement of one immutable OS image manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageEndorsement {
    /// SHA-256 digest bytes of the OCI manifest, never a mutable tag.
    pub image_manifest_sha256: [u8; 32],
    /// Monotonic Tilo generation embedded in the signed UKI.
    pub generation: u64,
    /// Time at which the device issued this endorsement.
    pub issued_at_ns: u64,
    /// Root-signed certificate for the device endorsing the image.
    pub device_certificate: DeviceCert,
    /// Separate root grant proving this device may endorse OS images.
    pub signing_authorization: ImageSigningAuthorization,
    /// Device Ed25519 signature over the domain-separated canonical view.
    #[serde(with = "crate::cert::serde_bytes_64")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct ImageEndorsementView<'a> {
    image_manifest_sha256: &'a [u8; 32],
    generation: u64,
    issued_at_ns: u64,
    device_certificate: &'a DeviceCert,
    signing_authorization: &'a ImageSigningAuthorization,
}

impl ImageEndorsement {
    fn view(&self) -> ImageEndorsementView<'_> {
        ImageEndorsementView {
            image_manifest_sha256: &self.image_manifest_sha256,
            generation: self.generation,
            issued_at_ns: self.issued_at_ns,
            device_certificate: &self.device_certificate,
            signing_authorization: &self.signing_authorization,
        }
    }
}

/// Image-endorsement signing and verification failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ImageEndorsementError {
    /// The embedded device certificate is invalid or expired.
    #[error("image signer certificate is invalid: {0}")]
    DeviceCertificate(#[from] DeviceCertError),
    /// The dedicated image-signing grant is invalid.
    #[error("image-signing authorization is invalid: {0}")]
    SigningAuthorization(#[from] ImageSigningAuthorizationError),
    /// The grant and device certificate identify different principals.
    #[error("image-signing authorization does not match the device certificate")]
    AuthorizationMismatch,
    /// The supplied signing key is not the certified device key.
    #[error("signing key does not match the certified device")]
    WrongDeviceKey,
    /// The endorsement claims to have been created in the future.
    #[error("image endorsement was issued in the future")]
    IssuedInFuture,
    /// The statement is validly signed but too old to establish freshness.
    #[error("image endorsement is older than the seven-day freshness window")]
    TooOld,
    /// Generation zero cannot participate in a monotonic release series.
    #[error("image endorsement generation must be greater than zero")]
    InvalidGeneration,
    /// The endorsement predates the certified device identity.
    #[error("image endorsement predates its device certificate")]
    IssuedBeforeDeviceCertificate,
    /// The certificate chains to a different human identity than the verifier trusts.
    #[error("image endorsement chains to an unexpected human identity")]
    WrongUserRoot,
    /// The device signature did not verify.
    #[error("image endorsement signature did not verify")]
    BadSignature,
    /// The signer appears in the trusted human root's current revocation set.
    #[error("image signer device has been revoked")]
    DeviceRevoked,
    /// Canonical serialization failed.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Failures while granting or validating image-signing authority.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ImageSigningAuthorizationError {
    /// The root key does not match the human identity named by the grant.
    #[error("root key does not match authorization identity")]
    WrongRootKey,
    /// The grant has an empty or backwards validity interval.
    #[error("image-signing authorization has an invalid validity interval")]
    InvalidValidity,
    /// The grant leaves a device able to sign unattended for too long.
    #[error("image-signing authorization exceeds the 30-day maximum lifetime")]
    ValidityTooLong,
    /// The grant is not valid at the checked time.
    #[error("image-signing authorization is not currently valid")]
    NotValid,
    /// The verifier trusts a different human root.
    #[error("image-signing authorization chains to an unexpected human identity")]
    WrongUserRoot,
    /// The human-root signature did not verify.
    #[error("image-signing authorization signature did not verify")]
    BadSignature,
    /// Canonical serialization failed.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

fn validate_image_signing_authorization_validity(
    authorization: &ImageSigningAuthorization,
) -> Result<(), ImageSigningAuthorizationError> {
    let lifetime = authorization
        .expires_at_ns
        .checked_sub(authorization.issued_at_ns)
        .filter(|lifetime| *lifetime > 0)
        .ok_or(ImageSigningAuthorizationError::InvalidValidity)?;
    if lifetime > MAX_IMAGE_SIGNING_AUTHORIZATION_LIFETIME_NS {
        return Err(ImageSigningAuthorizationError::ValidityTooLong);
    }
    Ok(())
}

/// Return the domain-separated digest the human root signs for an image grant.
///
/// # Errors
///
/// Returns an error if canonical serialization fails.
pub fn image_signing_authorization_digest(
    authorization: &ImageSigningAuthorization,
) -> Result<[u8; 32], ImageSigningAuthorizationError> {
    signing_digest(
        IMAGE_SIGNING_AUTHORIZATION_PREIMAGE_TAG,
        &authorization.view(),
    )
    .map_err(Into::into)
}

/// Grant one device key time-bounded authority to endorse OS images.
///
/// # Errors
///
/// Returns an error for the wrong human root, an invalid validity interval, or
/// canonical serialization failure.
pub fn sign_image_signing_authorization(
    authorization: &mut ImageSigningAuthorization,
    user_root: &ed25519_dalek::SigningKey,
) -> Result<(), ImageSigningAuthorizationError> {
    if user_root.verifying_key().to_bytes() != authorization.user_public_key {
        return Err(ImageSigningAuthorizationError::WrongRootKey);
    }
    validate_image_signing_authorization_validity(authorization)?;
    let digest = image_signing_authorization_digest(authorization)?;
    authorization.signature = user_root.sign(&digest).to_bytes();
    Ok(())
}

/// Verify a dedicated image-signing grant under an explicitly trusted root.
///
/// # Errors
///
/// Returns an error for the wrong root, invalid lifetime, expiry, or signature.
pub fn verify_image_signing_authorization_at(
    authorization: &ImageSigningAuthorization,
    trusted_user_root: &[u8; 32],
    now_ns: u64,
) -> Result<(), ImageSigningAuthorizationError> {
    if &authorization.user_public_key != trusted_user_root {
        return Err(ImageSigningAuthorizationError::WrongUserRoot);
    }
    validate_image_signing_authorization_validity(authorization)?;
    if now_ns < authorization.issued_at_ns || now_ns >= authorization.expires_at_ns {
        return Err(ImageSigningAuthorizationError::NotValid);
    }
    let root = ed25519_dalek::VerifyingKey::from_bytes(trusted_user_root)
        .map_err(|_| ImageSigningAuthorizationError::BadSignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(&authorization.signature);
    let digest = image_signing_authorization_digest(authorization)?;
    root.verify(&digest, &signature)
        .map_err(|_| ImageSigningAuthorizationError::BadSignature)
}

/// Return the exact digest an external/non-exportable device key must sign.
///
/// # Errors
///
/// Returns an error if canonical serialization fails.
pub fn image_endorsement_signing_digest(
    endorsement: &ImageEndorsement,
) -> Result<[u8; 32], ImageEndorsementError> {
    signing_digest(IMAGE_ENDORSEMENT_PREIMAGE_TAG, &endorsement.view()).map_err(Into::into)
}

/// Sign an image endorsement with the certified primary-device key.
///
/// # Errors
///
/// Returns an error if the device certificate is invalid at issuance, the
/// dedicated grant does not match it, the key does not match it, or canonical
/// serialization fails.
pub fn sign_image_endorsement(
    endorsement: &mut ImageEndorsement,
    device_key: &ed25519_dalek::SigningKey,
) -> Result<(), ImageEndorsementError> {
    if endorsement.generation == 0 {
        return Err(ImageEndorsementError::InvalidGeneration);
    }
    verify_device_cert_at(&endorsement.device_certificate, endorsement.issued_at_ns)?;
    if endorsement.issued_at_ns < endorsement.device_certificate.issued_at_ns {
        return Err(ImageEndorsementError::IssuedBeforeDeviceCertificate);
    }
    verify_image_signing_authorization_at(
        &endorsement.signing_authorization,
        &endorsement.signing_authorization.user_public_key,
        endorsement.issued_at_ns,
    )?;
    if endorsement.signing_authorization.user_public_key
        != endorsement.device_certificate.user_public_key
        || endorsement.signing_authorization.device_public_key
            != endorsement.device_certificate.device_public_key
    {
        return Err(ImageEndorsementError::AuthorizationMismatch);
    }
    if device_key.verifying_key().to_bytes() != endorsement.device_certificate.device_public_key {
        return Err(ImageEndorsementError::WrongDeviceKey);
    }
    let digest = image_endorsement_signing_digest(endorsement)?;
    endorsement.signature = device_key.sign(&digest).to_bytes();
    Ok(())
}

/// Verify the complete human-root → device → immutable-image chain.
///
/// # Errors
///
/// Returns an error if the certificate is invalid/expired, the device is not
/// authorized, the statement is future-dated, or its signature is invalid.
pub fn verify_image_endorsement_at(
    endorsement: &ImageEndorsement,
    trusted_user_root: &[u8; 32],
    now_ns: u64,
) -> Result<(), ImageEndorsementError> {
    if endorsement.generation == 0 {
        return Err(ImageEndorsementError::InvalidGeneration);
    }
    if &endorsement.device_certificate.user_public_key != trusted_user_root {
        return Err(ImageEndorsementError::WrongUserRoot);
    }
    let endorsement_age = now_ns
        .checked_sub(endorsement.issued_at_ns)
        .ok_or(ImageEndorsementError::IssuedInFuture)?;
    if endorsement_age > MAX_IMAGE_ENDORSEMENT_AGE_NS {
        return Err(ImageEndorsementError::TooOld);
    }
    verify_device_cert_at(&endorsement.device_certificate, now_ns)?;
    if endorsement.issued_at_ns < endorsement.device_certificate.issued_at_ns {
        return Err(ImageEndorsementError::IssuedBeforeDeviceCertificate);
    }
    verify_image_signing_authorization_at(
        &endorsement.signing_authorization,
        trusted_user_root,
        endorsement.issued_at_ns,
    )?;
    verify_image_signing_authorization_at(
        &endorsement.signing_authorization,
        trusted_user_root,
        now_ns,
    )?;
    if endorsement.signing_authorization.device_public_key
        != endorsement.device_certificate.device_public_key
    {
        return Err(ImageEndorsementError::AuthorizationMismatch);
    }
    let key =
        ed25519_dalek::VerifyingKey::from_bytes(&endorsement.device_certificate.device_public_key)
            .map_err(|_| ImageEndorsementError::BadSignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(&endorsement.signature);
    let digest = image_endorsement_signing_digest(endorsement)?;
    key.verify(&digest, &signature)
        .map_err(|_| ImageEndorsementError::BadSignature)
}

/// Verify an image endorsement and reject every currently revoked signer.
///
/// Endorsement issuance time is asserted by the device itself, not by an
/// external timestamp authority. Consequently a compromised device could
/// backdate a new endorsement; accepting signatures nominally made before the
/// revocation cut-off would not be safe here.
///
/// # Errors
///
/// Returns anything [`verify_image_endorsement_at`] can return, or
/// [`ImageEndorsementError::DeviceRevoked`] when the signer is revoked.
pub fn verify_image_endorsement_with_revocations_at(
    endorsement: &ImageEndorsement,
    trusted_user_root: &[u8; 32],
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), ImageEndorsementError> {
    verify_image_endorsement_at(endorsement, trusted_user_root, now_ns)?;
    if revocations.is_revoked(&endorsement.device_certificate.device_public_key) {
        return Err(ImageEndorsementError::DeviceRevoked);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceCapabilities, DeviceIdentity, UserIdentity, sign_device_cert};

    fn endorsement(
        capabilities: DeviceCapabilities,
        authorize_images: bool,
    ) -> ([u8; 32], ed25519_dalek::SigningKey, ImageEndorsement) {
        let (root, user) = UserIdentity::generate("Ada", 10);
        let (device, identity) = DeviceIdentity::generate("Builder", 11);
        let mut certificate = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: identity.public_key,
            device_label: identity.label,
            capabilities,
            issued_at_ns: 12,
            expires_at_ns: Some(100),
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, &root).unwrap();
        let mut authorization = ImageSigningAuthorization {
            user_public_key: user.public_key,
            device_public_key: identity.public_key,
            issued_at_ns: 15,
            expires_at_ns: 80,
            signature: [0; 64],
        };
        if authorize_images {
            sign_image_signing_authorization(&mut authorization, &root).unwrap();
        }
        (
            user.public_key,
            device,
            ImageEndorsement {
                image_manifest_sha256: [7; 32],
                generation: 42,
                issued_at_ns: 20,
                device_certificate: certificate,
                signing_authorization: authorization,
                signature: [0; 64],
            },
        )
    }

    /// A complete chain verifies and digest tampering fails; generation zero is rejected on both
    /// ends; signing refuses an invalid or not-yet-valid device certificate; old endorsements cannot
    /// extend a registry freeze; a revoked signer is rejected even when it backdates.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn image_trust_chains_verify_only_valid_current_signers() {
        // Phase: complete_chain_verifies_and_digest_tampering_fails.
        {
            let (root, device, mut image) = endorsement(DeviceCapabilities::primary(), true);
            sign_image_endorsement(&mut image, &device).unwrap();
            verify_image_endorsement_at(&image, &root, 50).unwrap();
            image.image_manifest_sha256[0] ^= 1;
            assert_eq!(
                verify_image_endorsement_at(&image, &root, 50),
                Err(ImageEndorsementError::BadSignature)
            );
            assert_eq!(
                verify_image_endorsement_at(&image, &[99; 32], 50),
                Err(ImageEndorsementError::WrongUserRoot)
            );
            image.issued_at_ns = 11;
            assert_eq!(
                verify_image_endorsement_at(&image, &root, 50),
                Err(ImageEndorsementError::IssuedBeforeDeviceCertificate)
            );
        }

        // Phase: generation_zero_is_rejected_before_signing_and_when_received.
        {
            let (root, device, mut image) = endorsement(DeviceCapabilities::primary(), true);
            image.generation = 0;
            assert_eq!(
                sign_image_endorsement(&mut image, &device),
                Err(ImageEndorsementError::InvalidGeneration)
            );
            assert_eq!(
                verify_image_endorsement_at(&image, &root, 50),
                Err(ImageEndorsementError::InvalidGeneration)
            );
        }

        // Phase: signing_refuses_an_invalid_or_not_yet_valid_device_certificate.
        {
            let (_root, device, mut image) = endorsement(DeviceCapabilities::primary(), true);
            image.device_certificate.signature[0] ^= 1;
            assert!(matches!(
                sign_image_endorsement(&mut image, &device),
                Err(ImageEndorsementError::DeviceCertificate(_))
            ));

            let (_root, device, mut image) = endorsement(DeviceCapabilities::primary(), true);
            image.issued_at_ns = image.device_certificate.issued_at_ns - 1;
            assert_eq!(
                sign_image_endorsement(&mut image, &device),
                Err(ImageEndorsementError::DeviceCertificate(
                    DeviceCertError::NotYetValid
                ))
            );
        }

        // Phase: old_endorsements_cannot_extend_a_registry_freeze.
        {
            let (root, device, mut image) = endorsement(DeviceCapabilities::primary(), true);
            sign_image_endorsement(&mut image, &device).unwrap();
            assert_eq!(
                verify_image_endorsement_at(
                    &image,
                    &root,
                    image.issued_at_ns + MAX_IMAGE_ENDORSEMENT_AGE_NS + 1,
                ),
                Err(ImageEndorsementError::TooOld)
            );
        }

        // Phase: revoked_signer_is_rejected_even_if_it_backdates_an_endorsement.
        {
            use crate::{RevocationEntry, RevocationReason, sign_revocation_entry};

            let (root_key, user) = crate::UserIdentity::generate("root", 1);
            let (device_key, identity) = crate::DeviceIdentity::generate("builder", 2);
            let mut cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: identity.public_key,
                device_label: identity.label,
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 3,
                expires_at_ns: Some(100),
                signature: [0; 64],
            };
            crate::sign_device_cert(&mut cert, &root_key).unwrap();
            let mut authorization = ImageSigningAuthorization {
                user_public_key: user.public_key,
                device_public_key: identity.public_key,
                issued_at_ns: 4,
                expires_at_ns: 90,
                signature: [0; 64],
            };
            sign_image_signing_authorization(&mut authorization, &root_key).unwrap();
            let mut image = ImageEndorsement {
                image_manifest_sha256: [9; 32],
                generation: 1,
                issued_at_ns: 10,
                device_certificate: cert,
                signing_authorization: authorization,
                signature: [0; 64],
            };
            sign_image_endorsement(&mut image, &device_key).unwrap();
            let mut revocation = RevocationEntry {
                user_public_key: user.public_key,
                revoked_device_public_key: identity.public_key,
                revoked_at_ns: 60,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut revocation, &root_key).unwrap();
            let mut revocations = RevocationList::new();
            revocations.insert(revocation).unwrap();
            assert_eq!(
                verify_image_endorsement_with_revocations_at(
                    &image,
                    &user.public_key,
                    &revocations,
                    50
                ),
                Err(ImageEndorsementError::DeviceRevoked)
            );
        }
    }

    /// Device admission does not imply image signing; a dedicated grant authorises an ordinary member
    /// and expires; grants cannot encode effectively unbounded authority.
    #[test]
    fn image_signing_authority_is_granted_explicitly_and_bounded() {
        // Phase: device_admission_authority_does_not_imply_image_signing_authority.
        {
            let (_root, device, mut image) = endorsement(DeviceCapabilities::primary(), false);
            assert!(matches!(
                sign_image_endorsement(&mut image, &device),
                Err(ImageEndorsementError::SigningAuthorization(_))
            ));
        }

        // Phase: dedicated_grant_can_authorize_an_ordinary_member_and_expires.
        {
            let (root, device, mut image) = endorsement(DeviceCapabilities::member(), true);
            sign_image_endorsement(&mut image, &device).unwrap();
            verify_image_endorsement_at(&image, &root, 50).unwrap();
            assert!(matches!(
                verify_image_endorsement_at(&image, &root, 80),
                Err(ImageEndorsementError::SigningAuthorization(
                    ImageSigningAuthorizationError::NotValid
                ))
            ));
        }

        // Phase: image_signing_grants_cannot_encode_effectively_unbounded_authority.
        {
            let (root, user) = UserIdentity::generate("root", 1);
            let (_device, identity) = DeviceIdentity::generate("builder", 2);
            let mut authorization = ImageSigningAuthorization {
                user_public_key: user.public_key,
                device_public_key: identity.public_key,
                issued_at_ns: 10,
                expires_at_ns: 10 + MAX_IMAGE_SIGNING_AUTHORIZATION_LIFETIME_NS + 1,
                signature: [0; 64],
            };
            assert_eq!(
                sign_image_signing_authorization(&mut authorization, &root),
                Err(ImageSigningAuthorizationError::ValidityTooLong)
            );

            // A malicious serializer cannot bypass the signing-side check: the
            // verifier rejects the same interval before considering its signature.
            assert_eq!(
                verify_image_signing_authorization_at(&authorization, &user.public_key, 11),
                Err(ImageSigningAuthorizationError::ValidityTooLong)
            );
        }
    }
}
