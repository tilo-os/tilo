//! TPM2 quotes that let an owner auto-approve one device.
//!
//! Auto-approval is a per-device setting the owner turns on. It holds the
//! boot chains the owner accepts, as PCR composite digests (PCR 4 + PCR 7,
//! SHA-256 bank, see `docs/security/boot-chain.md`). A request is
//! auto-approved only when it carries a quote that
//!
//! * is signed by the attestation key certified in the device's
//!   [`crate::BootKeyCert`] (a restricted TPM key, so the TPM produced it),
//! * carries [`qualifying_data`] for this exact request as `extraData`, so a
//!   recorded quote answers no other challenge, and
//! * reports a PCR composite the owner accepted.
//!
//! Anything else falls back to the owner's interactive approval.

use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::cert::BootKeyCert;
use crate::wire::Challenge;
use crate::{BootId, digest};

const QUALIFYING_CONTEXT: &str = "tilo-unlock attestation nonce v1";
const TPM_GENERATED_VALUE: u32 = 0xff54_4347;
const TPM_ST_ATTEST_QUOTE: u16 = 0x8018;
const TPM_ALG_SHA256: u16 = 0x000b;
/// The PCRs a quote must cover: the loaded images and the Secure Boot policy.
pub const QUOTED_PCRS: [u8; 2] = [4, 7];

/// A TPM2 quote over [`QUOTED_PCRS`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quote {
    /// The marshalled `TPMS_ATTEST` the TPM signed.
    #[serde(with = "serde_bytes")]
    pub attest: Vec<u8>,
    /// ECDSA P-256 / SHA-256 signature over `attest`, DER or `r || s`.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
    /// The quoted PCR values in ascending index order, SHA-256 bank.
    pub pcrs: Vec<(u8, [u8; 32])>,
}

/// Why a quote is not acceptable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum AttestError {
    /// The certificate names no attestation key, or it is not a P-256 key.
    #[error("no usable attestation key")]
    NoKey,
    /// The signature does not verify under the attestation key.
    #[error("quote signature did not verify")]
    BadSignature,
    /// The attest structure is not a TPM-generated quote.
    #[error("not a TPM quote")]
    NotAQuote,
    /// The quote was made for another request.
    #[error("quote answers another request")]
    WrongNonce,
    /// The PCR selection or values do not match the quote.
    #[error("quoted PCRs do not match")]
    PcrMismatch,
}

/// The 32 bytes early boot passes the TPM as qualifying data for this
/// request: the certificate, the answering sibling, the challenge nonce, the
/// boot id and the ephemeral key.
///
/// # Errors
///
/// [`AttestError::NotAQuote`] if the inputs cannot be encoded.
pub fn qualifying_data(
    cert: &BootKeyCert,
    challenge: &Challenge,
    boot_id: &BootId,
    ephemeral_public: &[u8; 32],
) -> Result<[u8; 32], AttestError> {
    let cert = cert.signing_digest().map_err(|_| AttestError::NotAQuote)?;
    digest(
        QUALIFYING_CONTEXT,
        &(
            cert,
            &challenge.sibling_public_key,
            &challenge.nonce,
            boot_id,
            ephemeral_public,
        ),
    )
    .map_err(|_| AttestError::NotAQuote)
}

/// The composite digest a quote reports for `pcrs`: SHA-256 over the values
/// in ascending index order. This is what the owner's accepted boot chains
/// list.
#[must_use]
pub fn pcr_composite(pcrs: &[(u8, [u8; 32])]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for (_, value) in pcrs {
        hash.update(value);
    }
    hash.finalize().into()
}

/// Verify `quote` under `attestation_key` (SEC1 P-256) for the request whose
/// qualifying data is `expected_nonce`, and return its PCR composite.
///
/// # Errors
///
/// As [`AttestError`].
pub fn verify_quote(
    attestation_key: &[u8],
    quote: &Quote,
    expected_nonce: &[u8; 32],
) -> Result<[u8; 32], AttestError> {
    let key = VerifyingKey::from_sec1_bytes(attestation_key).map_err(|_| AttestError::NoKey)?;
    let signature = if quote.signature.len() == 64 {
        Signature::from_slice(&quote.signature)
    } else {
        Signature::from_der(&quote.signature)
    }
    .map_err(|_| AttestError::BadSignature)?;
    key.verify(&quote.attest, &signature.normalize_s())
        .map_err(|_| AttestError::BadSignature)?;
    let parsed = parse_quote(&quote.attest)?;
    if parsed.extra_data != expected_nonce.as_slice() {
        return Err(AttestError::WrongNonce);
    }
    let indices: Vec<u8> = quote.pcrs.iter().map(|(index, _)| *index).collect();
    if parsed.selection != QUOTED_PCRS || indices != QUOTED_PCRS {
        return Err(AttestError::PcrMismatch);
    }
    let composite = pcr_composite(&quote.pcrs);
    if parsed.pcr_digest != composite.as_slice() {
        return Err(AttestError::PcrMismatch);
    }
    Ok(composite)
}

struct ParsedQuote<'a> {
    extra_data: &'a [u8],
    selection: Vec<u8>,
    pcr_digest: &'a [u8],
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], AttestError> {
        if self.0.len() < n {
            return Err(AttestError::NotAQuote);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], AttestError> {
        <[u8; N]>::try_from(self.take(N)?).map_err(|_| AttestError::NotAQuote)
    }

    fn u16(&mut self) -> Result<u16, AttestError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, AttestError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn sized(&mut self) -> Result<&'a [u8], AttestError> {
        let len = usize::from(self.u16()?);
        self.take(len)
    }
}

/// Parse a marshalled `TPMS_ATTEST` of type quote with one SHA-256 bank.
fn parse_quote(bytes: &[u8]) -> Result<ParsedQuote<'_>, AttestError> {
    let mut r = Reader(bytes);
    if r.u32()? != TPM_GENERATED_VALUE || r.u16()? != TPM_ST_ATTEST_QUOTE {
        return Err(AttestError::NotAQuote);
    }
    let _qualified_signer = r.sized()?;
    let extra_data = r.sized()?;
    // clockInfo (clock, resetCount, restartCount, safe), firmwareVersion.
    r.take(8 + 4 + 4 + 1 + 8)?;
    if r.u32()? != 1 || r.u16()? != TPM_ALG_SHA256 {
        return Err(AttestError::PcrMismatch);
    }
    let size = usize::from(r.array::<1>()?[0]);
    let bitmap = r.take(size)?;
    let mut selection = Vec::new();
    for (byte_index, byte) in bitmap.iter().enumerate() {
        for bit in 0..8_u8 {
            if byte & (1 << bit) != 0 {
                let index = byte_index
                    .checked_mul(8)
                    .and_then(|base| base.checked_add(usize::from(bit)))
                    .and_then(|index| u8::try_from(index).ok())
                    .ok_or(AttestError::PcrMismatch)?;
                selection.push(index);
            }
        }
    }
    let pcr_digest = r.sized()?;
    if !r.0.is_empty() {
        return Err(AttestError::NotAQuote);
    }
    Ok(ParsedQuote {
        extra_data,
        selection,
        pcr_digest,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use p256::ecdsa::{SigningKey, signature::Signer as _};

    /// A marshalled quote over `pcrs` with `extra` as qualifying data.
    pub(crate) fn attest_bytes(extra: &[u8], pcrs: &[(u8, [u8; 32])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&TPM_GENERATED_VALUE.to_be_bytes());
        out.extend_from_slice(&TPM_ST_ATTEST_QUOTE.to_be_bytes());
        out.extend_from_slice(&2_u16.to_be_bytes());
        out.extend_from_slice(&[0, 0xb]);
        out.extend_from_slice(&u16::try_from(extra.len()).unwrap().to_be_bytes());
        out.extend_from_slice(extra);
        out.extend_from_slice(&[0; 25]);
        out.extend_from_slice(&1_u32.to_be_bytes());
        out.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes());
        let mut bitmap = [0_u8; 3];
        for (index, _) in pcrs {
            bitmap[usize::from(*index / 8)] |= 1 << (index % 8);
        }
        out.push(3);
        out.extend_from_slice(&bitmap);
        let composite = pcr_composite(pcrs);
        out.extend_from_slice(&32_u16.to_be_bytes());
        out.extend_from_slice(&composite);
        out
    }

    pub(crate) fn tpm_key() -> SigningKey {
        SigningKey::from_slice(&[9_u8; 32]).unwrap()
    }

    pub(crate) fn sec1(key: &SigningKey) -> Vec<u8> {
        key.verifying_key().to_sec1_bytes().to_vec()
    }

    pub(crate) fn quote(key: &SigningKey, extra: &[u8], pcrs: &[(u8, [u8; 32])]) -> Quote {
        let attest = attest_bytes(extra, pcrs);
        let signature: Signature = key.sign(&attest);
        Quote {
            attest,
            signature: signature.to_der().as_bytes().to_vec(),
            pcrs: pcrs.to_vec(),
        }
    }

    #[test]
    fn quotes_bind_key_nonce_and_pcrs() {
        let key = tpm_key();
        let pcrs = [(4, [1; 32]), (7, [2; 32])];
        let nonce = [5; 32];
        let good = quote(&key, &nonce, &pcrs);
        assert_eq!(
            verify_quote(&sec1(&key), &good, &nonce),
            Ok(pcr_composite(&pcrs))
        );
        assert_eq!(
            verify_quote(&sec1(&key), &good, &[6; 32]),
            Err(AttestError::WrongNonce)
        );
        let other = SigningKey::from_slice(&[8_u8; 32]).unwrap();
        assert_eq!(
            verify_quote(&sec1(&other), &good, &nonce),
            Err(AttestError::BadSignature)
        );
        let mut lying = good.clone();
        lying.pcrs[0].1 = [3; 32];
        assert_eq!(
            verify_quote(&sec1(&key), &lying, &nonce),
            Err(AttestError::PcrMismatch)
        );
        let narrow = quote(&key, &nonce, &pcrs[1..]);
        assert_eq!(
            verify_quote(&sec1(&key), &narrow, &nonce),
            Err(AttestError::PcrMismatch)
        );
        let mut forged = good;
        forged.attest[4] = 0x80;
        forged.attest[5] = 0x17;
        assert_eq!(
            verify_quote(&sec1(&key), &forged, &nonce),
            Err(AttestError::BadSignature)
        );
    }
}
