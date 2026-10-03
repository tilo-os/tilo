//! The LUKS2 token that carries one generation's sealed unlock secret.
//!
//! cryptsetup stores unknown token types verbatim as long as they carry `type`
//! and `keyslots`, so a custom token needs no cryptsetup plugin to *store*. It
//! does need one to *use*, which is why unlocking goes through
//! `tilo-rollback unlock` rather than through `systemd-cryptsetup`'s built-in
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

use crate::policy::{GenerationPolicy, NvConstraint, Prediction};
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

/// LUKS2 token type string.
pub const TOKEN_TYPE: &str = "tilo-rollback";

/// Schema version, so a later change can be recognised rather than misparsed.
pub const TOKEN_VERSION: u32 = 2;

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
    /// Device-local PCRs applied after `PolicyAuthorize`.
    pub platform_pcrs: Vec<u32>,
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

impl From<&Prediction> for SignedPrediction {
    fn from(p: &Prediction) -> Self {
        Self {
            approved_policy: B64.encode(&p.approved_policy),
            signature: B64.encode(&p.signature),
            label: p.label.clone(),
        }
    }
}

/// The object `tpm2_create` sealed, and the policy digest it was sealed to.
#[derive(Debug, Clone, Copy)]
pub struct SealedObject<'a> {
    /// `TPM2B_PUBLIC`.
    pub public: &'a [u8],
    /// `TPM2B_PRIVATE`.
    pub private: &'a [u8],
    pub policy_digest: &'a [u8],
}

impl RollbackToken {
    /// `nv_base` is the counter value for floor generation 0 that `policy.nv.operand` was
    /// derived from.
    pub fn new(
        keyslot: u32,
        policy: &GenerationPolicy<'_>,
        nv_base: i64,
        sealed: &SealedObject<'_>,
        predictions: &[Prediction],
    ) -> Self {
        Self {
            token_type: TOKEN_TYPE.into(),
            keyslots: vec![keyslot.to_string()],
            version: TOKEN_VERSION,
            generation: policy.generation,
            nv_index: policy.nv.index,
            nv_operand: policy.nv.operand,
            nv_base,
            pcr_bank: policy.pcr_bank.into(),
            pcrs: policy.pcrs.to_vec(),
            platform_pcrs: policy.platform_pcrs.to_vec(),
            pin: policy.pin,
            sealed_public: B64.encode(sealed.public),
            sealed_private: B64.encode(sealed.private),
            key_name: B64.encode(policy.key_name),
            policy_digest: B64.encode(sealed.policy_digest),
            predictions: predictions.iter().map(SignedPrediction::from).collect(),
        }
    }

    /// The policy this token's secret is sealed under, given its decoded
    /// [`key_name`](Self::decoded_key_name).
    pub fn policy<'a>(&'a self, key_name: &'a [u8]) -> GenerationPolicy<'a> {
        GenerationPolicy {
            generation: self.generation,
            key_name,
            pcr_bank: &self.pcr_bank,
            pcrs: &self.pcrs,
            platform_pcrs: &self.platform_pcrs,
            nv: NvConstraint {
                index: self.nv_index,
                operand: self.nv_operand,
            },
            pin: self.pin,
        }
    }

    /// The keyslot this token is bound to.
    ///
    /// # Errors
    ///
    /// Returns an error if the token has no keyslot binding, or if the binding is not a
    /// parseable number.
    pub fn keyslot(&self) -> Result<u32> {
        let first = self
            .keyslots
            .first()
            .context("token has no keyslot binding")?;
        first
            .parse()
            .with_context(|| format!("token keyslot {first:?} is not a number"))
    }

    /// # Errors
    ///
    /// Returns an error if `sealed_public` is not valid base64.
    pub fn decoded_public(&self) -> Result<Vec<u8>> {
        B64.decode(&self.sealed_public)
            .context("token sealed_public is not base64")
    }

    /// # Errors
    ///
    /// Returns an error if `sealed_private` is not valid base64.
    pub fn decoded_private(&self) -> Result<Vec<u8>> {
        B64.decode(&self.sealed_private)
            .context("token sealed_private is not base64")
    }

    /// # Errors
    ///
    /// Returns an error if `key_name` is not valid base64.
    pub fn decoded_key_name(&self) -> Result<Vec<u8>> {
        B64.decode(&self.key_name)
            .context("token key_name is not base64")
    }

    /// # Errors
    ///
    /// Returns an error if any prediction's `approved_policy` or `signature` is not valid
    /// base64.
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

    /// # Errors
    ///
    /// Returns an error if the token type or schema version is unrecognised, if it carries no
    /// signed prediction, if `nv_operand` is inconsistent with `nv_base`/`generation`, or if
    /// [`Self::keyslot`] fails.
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
        if self.pcrs.is_empty() || self.platform_pcrs.is_empty() {
            bail!("token PCR selections must include authorized and platform PCRs");
        }
        if self.pcrs.iter().any(|p| self.platform_pcrs.contains(p)) {
            bail!("authorized and platform PCR selections must not overlap");
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

    /// # Errors
    ///
    /// Returns an error if serialisation fails (effectively unreachable for this type).
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).context("serialising the rollback token")
    }

    /// # Errors
    ///
    /// Returns an error if `s` is not valid JSON for this schema, or if the decoded token
    /// fails [`Self::validate`].
    pub fn from_json(s: &str) -> Result<Self> {
        let t: Self = serde_json::from_str(s).context("parsing a tilo-rollback LUKS2 token")?;
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
            &GenerationPolicy {
                generation: 7,
                key_name: b"name",
                pcr_bank: "sha256",
                pcrs: &[11],
                platform_pcrs: &[7],
                nv: NvConstraint {
                    index: 0x01D9_1100,
                    operand: 107,
                },
                pin: true,
            },
            100,
            &SealedObject {
                public: b"pub",
                private: b"priv",
                policy_digest: b"digest",
            },
            &[Prediction {
                approved_policy: b"ap".to_vec(),
                signature: b"sig".to_vec(),
                label: "as built".into(),
            }],
        )
    }

    #[test]
    fn tokens_round_trip_with_named_fields_and_reject_inconsistency() {
        // Phase `round_trips_through_json`.
        {
            let t = sample();
            let j = t.to_json().unwrap();
            assert_eq!(RollbackToken::from_json(&j).unwrap(), t);
        }
        // Phase `cryptsetup_required_fields_are_present_and_named_right`.
        {
            let j = sample().to_json().unwrap();
            let v: serde_json::Value = serde_json::from_str(&j).unwrap();
            assert_eq!(v["type"], "tilo-rollback");
            assert_eq!(v["keyslots"], serde_json::json!(["2"]));
        }
        // Phase `blobs_decode_back_to_bytes`.
        {
            let t = sample();
            assert_eq!(t.decoded_public().unwrap(), b"pub");
            assert_eq!(t.decoded_private().unwrap(), b"priv");
            assert_eq!(t.decoded_key_name().unwrap(), b"name");
            let p = t.decoded_predictions().unwrap();
            assert_eq!(p[0].approved_policy, b"ap");
            assert_eq!(p[0].signature, b"sig");
        }
        // Phase `keyslot_is_parsed_from_the_string_form`.
        {
            assert_eq!(sample().keyslot().unwrap(), 2);
        }
        // Phase `a_token_with_no_prediction_is_rejected`.
        {
            let mut t = sample();
            t.predictions.clear();
            assert!(t.validate().is_err());
        }
        // Phase `an_inconsistent_operand_is_rejected`.
        {
            let mut t = sample();
            t.nv_operand = 999;
            let e = t.validate().unwrap_err().to_string();
            assert!(e.contains("internally inconsistent"), "{e}");
        }
        // Phase `a_future_schema_version_is_rejected_not_guessed_at`.
        {
            let mut t = sample();
            t.version = TOKEN_VERSION + 1;
            assert!(t.validate().is_err());
        }
    }
}
