//! The LUKS2 token that carries one generation's sealed unlock secret.
//!
//! cryptsetup stores unknown token types verbatim as long as they carry `type`
//! and `keyslots`, so a custom token needs no cryptsetup plugin to *store*. It
//! does need one to *use*, which is why unlocking goes through
//! `qos-rollback unlock` rather than through `systemd-cryptsetup`'s built-in
//! TPM2 handling — see the module notes in [`crate::keyslots`].
//!
//! # What is in here and why
//!
//! Everything needed to unseal, so that a rollback works from the disk header
//! alone with no help from the (possibly unbootable) root filesystem:
//!
//! * the sealed object's public and private blobs;
//! * the generation, which is both the `PolicyNV` bound and the
//!   `PolicyAuthorize` policy reference;
//! * the absolute NV operand, and the `base` it was derived from;
//! * the PCR bank and selection;
//! * one or more signed predictions, tried in order.
//!
//! # None of it is secret, and none of it is trusted
//!
//! The LUKS2 header is plaintext metadata. Everything here is either public
//! (predictions, signatures, policy digests) or useless without the TPM (the
//! sealed blobs). Nor does editing any of it help an attacker: the bound that
//! gates the unseal is inside the sealed object's `authPolicy`, which is
//! cryptographically fixed at seal time. Changing `generation` or `nv_operand`
//! in the JSON changes which policy the unlock path *builds*, and a policy that
//! does not match `authPolicy` simply fails.
//!
//! The one thing header tampering can do is deny service — see the note on
//! `base` in [`crate::counter`], and [`crate::counter::MAX_INCREMENTS_PER_ADVANCE`].

use crate::policy::Prediction;
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

/// LUKS2 token type string.
pub const TOKEN_TYPE: &str = "qos-rollback";

/// Schema version, so a later change can be recognised rather than misparsed.
pub const TOKEN_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedPrediction {
    /// base64 policy digest a trial `TPM2_PolicyPCR` reaches over the predicted
    /// measurements.
    pub approved_policy: String,
    /// base64 signature over `approved_policy || generation_be64`.
    pub signature: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackToken {
    /// Always [`TOKEN_TYPE`]; cryptsetup requires the field.
    #[serde(rename = "type")]
    pub token_type: String,
    /// Keyslots this token unlocks. cryptsetup requires the field, as strings.
    pub keyslots: Vec<String>,

    pub version: u32,
    /// Generation this token belongs to.
    pub generation: u64,
    /// NV index holding the rollback floor.
    pub nv_index: u32,
    /// Absolute `operandB` in the token's `TPM2_PolicyNV`: `base + generation`.
    pub nv_operand: u64,
    /// Counter value corresponding to floor generation 0. Signed, and usually
    /// negative — see [`crate::counter`].
    pub nv_base: i64,
    pub pcr_bank: String,
    pub pcrs: Vec<u32>,
    /// Is a PIN part of the policy?
    pub pin: bool,
    /// base64 `TPM2B_PUBLIC` of the sealed object.
    pub sealed_public: String,
    /// base64 `TPM2B_PRIVATE` of the sealed object.
    pub sealed_private: String,
    /// base64 `TPM2B_NAME` of the prediction authority's public key.
    pub key_name: String,
    /// base64 seal policy digest. Diagnostics only — the authoritative copy is
    /// inside `sealed_public`.
    pub policy_digest: String,
    pub predictions: Vec<SignedPrediction>,
}

impl RollbackToken {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        keyslot: u32,
        generation: u64,
        nv_index: u32,
        nv_operand: u64,
        nv_base: i64,
        pcr_bank: &str,
        pcrs: &[u32],
        pin: bool,
        sealed_public: &[u8],
        sealed_private: &[u8],
        key_name: &[u8],
        policy_digest: &[u8],
        predictions: &[Prediction],
    ) -> Self {
        Self {
            token_type: TOKEN_TYPE.into(),
            keyslots: vec![keyslot.to_string()],
            version: TOKEN_VERSION,
            generation,
            nv_index,
            nv_operand,
            nv_base,
            pcr_bank: pcr_bank.into(),
            pcrs: pcrs.to_vec(),
            pin,
            sealed_public: B64.encode(sealed_public),
            sealed_private: B64.encode(sealed_private),
            key_name: B64.encode(key_name),
            policy_digest: B64.encode(policy_digest),
            predictions: predictions
                .iter()
                .map(|p| SignedPrediction {
                    approved_policy: B64.encode(&p.approved_policy),
                    signature: B64.encode(&p.signature),
                    label: p.label.clone(),
                })
                .collect(),
        }
    }

    pub fn keyslot(&self) -> Result<u32> {
        let first = self
            .keyslots
            .first()
            .context("token has no keyslot binding")?;
        first
            .parse()
            .with_context(|| format!("token keyslot {first:?} is not a number"))
    }

    pub fn decoded_public(&self) -> Result<Vec<u8>> {
        B64.decode(&self.sealed_public)
            .context("token sealed_public is not base64")
    }
    pub fn decoded_private(&self) -> Result<Vec<u8>> {
        B64.decode(&self.sealed_private)
            .context("token sealed_private is not base64")
    }
    pub fn decoded_key_name(&self) -> Result<Vec<u8>> {
        B64.decode(&self.key_name)
            .context("token key_name is not base64")
    }

    pub fn decoded_predictions(&self) -> Result<Vec<Prediction>> {
        self.predictions
            .iter()
            .map(|p| {
                Ok(Prediction {
                    approved_policy: B64
                        .decode(&p.approved_policy)
                        .context("prediction approved_policy is not base64")?,
                    signature: B64
                        .decode(&p.signature)
                        .context("prediction signature is not base64")?,
                    label: p.label.clone(),
                })
            })
            .collect()
    }

    pub fn validate(&self) -> Result<()> {
        if self.token_type != TOKEN_TYPE {
            bail!("token type {:?} is not {TOKEN_TYPE}", self.token_type);
        }
        if self.version != TOKEN_VERSION {
            bail!(
                "token schema version {} is not the {TOKEN_VERSION} this build understands",
                self.version
            );
        }
        if self.predictions.is_empty() {
            bail!(
                "token for generation {} carries no signed prediction, so it can never satisfy \
                 PolicyAuthorize",
                self.generation
            );
        }
        // `nv_operand` must equal `nv_base + generation`, or the token was built
        // by something that does not agree with us about the mapping.
        let expect = crate::counter::operand(self.nv_base, self.generation)
            .context("token nv_base + generation is not a usable NV counter value")?;
        if self.nv_operand != expect {
            bail!(
                "token for generation {} is internally inconsistent: nv_operand {} but nv_base {} \
                 implies {expect}",
                self.generation,
                self.nv_operand,
                self.nv_base
            );
        }
        self.keyslot()?;
        Ok(())
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).context("serialising the rollback token")
    }

    pub fn from_json(s: &str) -> Result<Self> {
        let t: Self = serde_json::from_str(s).context("parsing a qos-rollback LUKS2 token")?;
        t.validate()?;
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RollbackToken {
        RollbackToken::new(
            2,
            7,
            0x01D9_1100,
            107,
            100,
            "sha256",
            &[7, 11],
            true,
            b"pub",
            b"priv",
            b"name",
            b"digest",
            &[Prediction {
                approved_policy: b"ap".to_vec(),
                signature: b"sig".to_vec(),
                label: "as built".into(),
            }],
        )
    }

    #[test]
    fn round_trips_through_json() {
        let t = sample();
        let j = t.to_json().unwrap();
        assert_eq!(RollbackToken::from_json(&j).unwrap(), t);
    }

    #[test]
    fn cryptsetup_required_fields_are_present_and_named_right() {
        let j = sample().to_json().unwrap();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["type"], "qos-rollback");
        assert_eq!(v["keyslots"], serde_json::json!(["2"]));
    }

    #[test]
    fn blobs_decode_back_to_bytes() {
        let t = sample();
        assert_eq!(t.decoded_public().unwrap(), b"pub");
        assert_eq!(t.decoded_private().unwrap(), b"priv");
        assert_eq!(t.decoded_key_name().unwrap(), b"name");
        let p = t.decoded_predictions().unwrap();
        assert_eq!(p[0].approved_policy, b"ap");
        assert_eq!(p[0].signature, b"sig");
    }

    #[test]
    fn keyslot_is_parsed_from_the_string_form() {
        assert_eq!(sample().keyslot().unwrap(), 2);
    }

    #[test]
    fn a_token_with_no_prediction_is_rejected() {
        let mut t = sample();
        t.predictions.clear();
        assert!(t.validate().is_err());
    }

    #[test]
    fn an_inconsistent_operand_is_rejected() {
        let mut t = sample();
        t.nv_operand = 999;
        let e = t.validate().unwrap_err().to_string();
        assert!(e.contains("internally inconsistent"), "{e}");
    }

    #[test]
    fn a_future_schema_version_is_rejected_not_guessed_at() {
        let mut t = sample();
        t.version = TOKEN_VERSION + 1;
        assert!(t.validate().is_err());
    }
}
