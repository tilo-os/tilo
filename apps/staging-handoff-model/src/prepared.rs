//! The public TPM sidecar for a prepared setup, and the release-catalog
//! descriptor that selects the target boot policy it is sealed against.
//!
//! This is the shared wire format. The preparation side writes it and the
//! installed boot reads it, so both sides agree on exactly these fields and
//! neither accepts a field the other refuses. Nothing here touches a TPM, a
//! file or a privileged caller: sealing, unsealing, reading a caller-owned
//! package and verifying a signed target against the release authority stay
//! with the platform backend that owns them.
//!
//! A prepared setup must not write another computer's identity, account tokens
//! or file ciphertext into a package protected only by a login password chosen
//! later. On the same computer the target boot artifact is already signed, so
//! the package secret is bound to all of:
//!
//! * the exact signed target UKI (its `.cmdline`, `.linux`, `.initrd` and
//!   every other measured section) through the release authority's signed
//!   PCR11 prediction;
//! * this actual TPM, through `TPM2_PolicyAuthorize` and the machine-local
//!   platform PCRs (PCR 7), never a presence check on a TPM device node;
//! * the endorsement key of the TPM that sealed it, recorded as its TPM Name
//!   so a copied sidecar names the wrong receiver before any policy session.
//!
//! The only secret in the sidecar is a `TPM2B_PRIVATE` blob; the package
//! itself stays exactly the canonical password-authenticated prepared setup,
//! encrypted with a high-entropy machine-generated password. The person's
//! first-boot login password is independent of it.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema of the public sidecar. Version 2 binds the original caller, the
/// receiving account's UID, a fresh seal nonce and a digest over every public
/// field, so a version 1 sidecar is refused rather than silently accepted with
/// weaker binding.
pub const SCHEMA: u32 = 2;
/// The sidecar travels beside the canonical prepared package.
pub const SIDECAR_FILE: &str = "tpm-handoff.json";
/// The target UKI and its signed prediction sidecars inside a target bundle.
pub const TARGET_UKI_FILE: &str = "tilo-target.efi";
pub const PREDICTIONS_DIR: &str = "predictions";
/// The largest sidecar any reader accepts. The setup handoff copies the
/// sidecar beside the package under the same bound, so a sidecar a writer
/// could produce but the handoff would drop is refused here instead.
pub const MAX_SIDECAR: u64 = 64 * 1024;
/// The same bound as a `usize`, for readers that accumulate bytes in memory.
pub const MAX_SIDECAR_BYTES: usize = 64 * 1024;
/// The largest target UKI a bundle may carry.
pub const MAX_UKI: u64 = 512 * 1024 * 1024;
/// The distinct, release-predictable systemd-stub PCRs a target policy names.
pub const PCRS: [u32; 1] = [11];
/// Machine-local PCRs applied after `PolicyAuthorize`.
pub const PLATFORM_PCRS: [u32; 1] = [7];
/// A signed prediction's signature is a small RSA signature.
pub const MAX_SIGNATURE_BYTES: usize = 4 * 1024;
/// The first installed account is UID 1000. The sidecar names it so first boot
/// cannot hand the package secret to a different account.
pub const RECEIVER_UID: u32 = 1000;

const BINDING_DOMAIN: &[u8] = b"tilo-prepared-tpm-binding-v2\0";

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decode a standard-base64 wire field, refusing an empty or oversized value.
///
/// # Errors
///
/// `value` is not standard base64, or it decodes to an empty value or one
/// longer than `limit`.
pub fn unb64(value: &str, what: &str, limit: usize) -> std::io::Result<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| invalid(format!("{what} is not base64")))?;
    if bytes.is_empty() || bytes.len() > limit {
        return Err(invalid(format!("{what} has an invalid length")));
    }
    Ok(bytes)
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// A target policy bundle published by the signed release catalog: the small
/// download a preparation host reviews instead of mounting the live installer
/// image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPolicyBundle {
    pub generation: u64,
    pub target_uki_sha256: [u8; 32],
    pub archive_sha256: String,
    pub archive_size: u64,
    pub url: String,
}

/// The signed target boot policy the prepared secret is sealed against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedTarget {
    pub schema: u32,
    pub generation: u64,
    pub pcr_bank: String,
    pub pcrs: Vec<u32>,
    pub platform_pcrs: Vec<u32>,
    pub approved_policy: String,
    pub signature: String,
    pub target_uki_sha256: [u8; 32],
    pub authority_sha256: [u8; 32],
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl SignedTarget {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn target_uki_sha256(&self) -> [u8; 32] {
        self.target_uki_sha256
    }

    /// Check the policy shape and that its prediction fields decode. Signature
    /// verification against the release authority belongs to the platform
    /// backend, which owns the authority.
    ///
    /// # Errors
    ///
    /// The schema, bank, PCR selection or digests are unsupported, or the
    /// prediction fields are not bounded base64.
    pub fn validate(&self) -> std::io::Result<()> {
        if self.schema != SCHEMA
            || self.pcr_bank != "sha256"
            || self.pcrs.as_slice() != PCRS
            || self.platform_pcrs.as_slice() != PLATFORM_PCRS
            || self.target_uki_sha256 == [0; 32]
            || self.authority_sha256 == [0; 32]
        {
            return Err(invalid("signed target policy is not the supported shape"));
        }
        let _ = unb64(&self.approved_policy, "approved policy", 64)?;
        let _ = unb64(&self.signature, "prediction signature", MAX_SIGNATURE_BYTES)?;
        Ok(())
    }
}

#[derive(Serialize)]
struct BindingView<'a> {
    schema: u32,
    policy: &'a SignedTarget,
    source_public_key: [u8; 32],
    package_sha256: [u8; 32],
    receiver_uid: u32,
    receiver_tpm_name: &'a str,
    key_name: &'a str,
    sealed_public: &'a str,
    sealed_private: &'a str,
    policy_digest: &'a str,
    seal_nonce: &'a str,
}

/// The public sidecar: policy, receiver identity and the TPM's sealed private
/// blob. No plaintext secret and no password are present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedPreparation {
    pub schema: u32,
    pub policy: SignedTarget,
    pub source_public_key: [u8; 32],
    pub package_sha256: [u8; 32],
    pub receiver_uid: u32,
    pub receiver_tpm_name: String,
    pub key_name: String,
    pub sealed_public: String,
    pub sealed_private: String,
    pub policy_digest: String,
    /// A fresh random value from the privileged seal. It distinguishes two
    /// seals of the same package even before any TPM operation, while the PCRs,
    /// generation and package digest are what stop a replay onto another boot
    /// state or package.
    pub seal_nonce: String,
    /// Digest over every public field including `seal_nonce`. Tampering with a
    /// copied sidecar is refused before the TPM is asked to unseal it.
    pub binding_sha256: String,
}

impl SealedPreparation {
    pub fn source_public_key(&self) -> [u8; 32] {
        self.source_public_key
    }

    pub fn package_sha256(&self) -> [u8; 32] {
        self.package_sha256
    }

    pub fn generation(&self) -> u64 {
        self.policy.generation
    }

    pub fn receiver_uid(&self) -> u32 {
        self.receiver_uid
    }

    fn binding_digest(&self) -> std::io::Result<[u8; 32]> {
        let bytes = serde_json::to_vec(&BindingView {
            schema: self.schema,
            policy: &self.policy,
            source_public_key: self.source_public_key,
            package_sha256: self.package_sha256,
            receiver_uid: self.receiver_uid,
            receiver_tpm_name: &self.receiver_tpm_name,
            key_name: &self.key_name,
            sealed_public: &self.sealed_public,
            sealed_private: &self.sealed_private,
            policy_digest: &self.policy_digest,
            seal_nonce: &self.seal_nonce,
        })
        .map_err(std::io::Error::other)?;
        let mut hasher = Sha256::new();
        hasher.update(BINDING_DOMAIN);
        hasher.update(bytes);
        Ok(hasher.finalize().into())
    }

    /// Recompute the public binding digest after the fields are filled in.
    ///
    /// # Errors
    ///
    /// A field cannot be serialized for the binding digest.
    pub fn seal_binding(&mut self) -> std::io::Result<()> {
        self.binding_sha256 = b64(&self.binding_digest()?);
        Ok(())
    }

    /// # Errors
    ///
    /// The sidecar is malformed, edited after sealing, or names an unsupported policy.
    pub fn validate(&self) -> std::io::Result<()> {
        if self.schema != SCHEMA
            || self.source_public_key == [0; 32]
            || self.package_sha256 == [0; 32]
            || self.receiver_uid != RECEIVER_UID
        {
            return Err(invalid("prepared TPM handoff is incomplete"));
        }
        self.policy.validate()?;
        let _ = unb64(&self.receiver_tpm_name, "receiver TPM name", 128)?;
        let _ = unb64(&self.key_name, "prediction authority name", 128)?;
        let _ = unb64(&self.sealed_public, "sealed public area", 4096)?;
        let _ = unb64(&self.sealed_private, "sealed private area", 4096)?;
        let _ = unb64(&self.policy_digest, "sealed policy digest", 64)?;
        let nonce = unb64(&self.seal_nonce, "seal nonce", 64)?;
        if nonce.len() != 32 {
            return Err(invalid(
                "prepared TPM handoff seal nonce has an invalid length",
            ));
        }
        if unb64(&self.binding_sha256, "sidecar binding digest", 64)? != self.binding_digest()? {
            return Err(invalid(
                "prepared TPM handoff fields were changed after sealing",
            ));
        }
        Ok(())
    }

    /// # Errors
    ///
    /// The sidecar is invalid or exceeds its accepted bound.
    pub fn encode(&self) -> std::io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        if bytes.len() as u64 > MAX_SIDECAR {
            return Err(invalid("prepared TPM handoff exceeds its bound"));
        }
        Ok(bytes)
    }

    /// # Errors
    ///
    /// The bytes are oversized, malformed, or fail the public binding check.
    pub fn decode(bytes: &[u8]) -> std::io::Result<Self> {
        if bytes.len() as u64 > MAX_SIDECAR {
            return Err(invalid("prepared TPM handoff exceeds its bound"));
        }
        let value: Self = serde_json::from_slice(bytes)
            .map_err(|_| invalid("prepared TPM handoff is malformed"))?;
        value.validate()?;
        Ok(value)
    }
}
