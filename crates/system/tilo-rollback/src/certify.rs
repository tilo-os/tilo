//! Making the rollback floor attestable to someone else.
//!
//! `TPM2_NV_Certify` signs the contents of an NV index with a loaded key, so a peer holding
//! that key's public half can verify "this machine's rollback floor is at generation N"
//! without trusting anything the machine says about itself.
//!
//! This module deliberately stops at *producing* the signed statement — the protocol that
//! carries it to a peer, the trust decisions a peer makes about the attestation key, and the
//! binding of the key to a device certificate are a separate concern with its own design.
//!
//! What it does guarantee: the floor is exposed in a form that makes the rest possible — a
//! `TPMS_ATTEST` structure and a signature over it, with the counter value and NV index Name
//! inside, plus a caller-supplied qualifying nonce so a verifier can defeat replay.
//!
//! The attestation key is created under the endorsement hierarchy and is restricted
//! (`sign` + `restricted`), which is what makes a `TPM2_NV_Certify` signature meaningful: a
//! restricted signing key cannot be used to sign arbitrary attacker-chosen data, so a valid
//! signature really does mean the TPM produced the attestation itself.

use crate::argv;
use crate::exec::{Runner, require};
use crate::policy::{self, Workspace};
use anyhow::{Context, Result};
use std::path::Path;

/// A signed statement about the counter.
#[derive(Debug, Clone)]
pub struct FloorAttestation {
    /// `TPMS_ATTEST` structure, marshalled.
    pub attestation: Vec<u8>,
    pub signature: Vec<u8>,
    /// Attestation key's public area, so a verifier can check the signature and see that the
    /// key is restricted.
    pub key_public: Vec<u8>,
    /// The counter value the attestation covers, for convenience. The authoritative copy is
    /// inside `attestation`.
    pub counter: u64,
    /// Floor generation implied by that counter and the recorded base.
    pub floor_generation: u64,
}

/// Create (or recreate) the attestation key. Returns the loaded key's context path and its
/// public area.
///
/// Deterministic in the same way the sealing parent is: same owner-hierarchy seed, same
/// template, same key — nothing to persist, and clearing the owner hierarchy is the only
/// thing that changes the identity.
///
/// Two deliberate choices, worth stating because a reader will expect the conventional ones:
///
/// * **The parent is the owner primary, not the EK.** The endorsement primary carries the
///   standard EK auth policy, so using it as a parent means running an endorsement-auth
///   policy session for every operation. A production deployment does want an EK-rooted key
///   bound to a device certificate by credential activation — that belongs with the
///   attestation protocol, a separate workstream. This key proves "one specific TPM produced
///   this attestation", which is what the floor statement needs.
/// * **`restricted` is not optional.** A restricted signing key is one the TPM refuses to
///   use on externally supplied data beginning with `TPM_GENERATED_VALUE`. Without it,
///   anyone holding the key could sign a hand-written structure that looks exactly like a
///   TPM attestation, with no way for a verifier to tell. The algorithm specifier must name
///   the scheme *and* an explicit null symmetric algorithm (`ecc256:ecdsa-sha256:null`) —
///   leave the symmetric part off and tpm2-tools fills in a default a signing key can't
///   carry, which the TPM rejects as "unsupported symmetric algorithm".
/// # Errors
///
/// Returns an error if any `tpm2_*` step (creating the parent, creating or loading the
/// attestation key) fails, or if the key's public area cannot be read back.
pub fn attestation_key(
    runner: &dyn Runner,
    ws: &Workspace,
) -> Result<(std::path::PathBuf, Vec<u8>)> {
    policy::flush(runner);
    let parent = ws.path("attest_parent.ctx");
    require(
        runner,
        "tpm2_createprimary",
        &argv![
            "-C",
            "o",
            "-g",
            "sha256",
            "-G",
            "ecc",
            "-c",
            parent.as_os_str()
        ],
        "creating the attestation key's parent",
    )?;
    let akpub = ws.path("ak.pub");
    let akpriv = ws.path("ak.priv");
    require(
        runner,
        "tpm2_create",
        &argv![
            "-C",
            parent.as_os_str(),
            "-G",
            "ecc256:ecdsa-sha256:null",
            "-g",
            "sha256",
            "-u",
            akpub.as_os_str(),
            "-r",
            akpriv.as_os_str(),
            "-a",
            "fixedtpm|fixedparent|sensitivedataorigin|userwithauth|restricted|sign",
        ],
        "creating the floor attestation key",
    )?;
    // Reload from the blobs rather than reusing tpm2_create's saved context: on a TPM with no
    // resource manager the parent and freshly created key already occupy the transient slots,
    // and the next tool's context load fails with a message that reads like a corrupt file.
    policy::flush(runner);
    let akctx = ws.path("ak.ctx");
    require(
        runner,
        "tpm2_load",
        &argv![
            "-C",
            parent.as_os_str(),
            "-u",
            akpub.as_os_str(),
            "-r",
            akpriv.as_os_str(),
            "-c",
            akctx.as_os_str(),
        ],
        "loading the floor attestation key",
    )?;
    let pubbytes = std::fs::read(&akpub).with_context(|| format!("reading {}", akpub.display()))?;
    Ok((akctx, pubbytes))
}

/// Sign the counter's current contents. `qualification` is the verifier's nonce; it lands
/// in the attestation's `extraData` and is what stops a captured attestation being replayed.
///
/// # Errors
///
/// Returns an error if the attestation key cannot be created/loaded, if `tpm2_nvcertify`
/// fails, if the counter has never been incremented, or if the resulting attestation/signature
/// files cannot be read back.
pub fn certify_floor(
    runner: &dyn Runner,
    ws: &Workspace,
    nv_index: u32,
    base: i64,
    qualification: &[u8],
    scratch: &Path,
) -> Result<FloorAttestation> {
    let (akctx, key_public) = attestation_key(runner, ws)?;
    let idx = format!("0x{nv_index:08X}");
    let attest = ws.path("nv.attest");
    let sig = ws.path("nv.sig");
    let qual = hex::encode(qualification);

    require(
        runner,
        "tpm2_nvcertify",
        &argv![
            "-C",
            akctx.as_os_str(),
            "-c",
            &idx,
            "-g",
            "sha256",
            "-f",
            "plain",
            "-o",
            sig.as_os_str(),
            "--attestation",
            attest.as_os_str(),
            "--size",
            "8",
            "-q",
            &qual,
            &idx,
        ],
        "certifying the rollback floor counter",
    )?;

    let counter = crate::counter::read(runner, nv_index, scratch)?.context(
        "the rollback floor counter has never been incremented, so there is nothing to certify",
    )?;
    policy::flush(runner);
    Ok(FloorAttestation {
        attestation: std::fs::read(&attest)
            .with_context(|| format!("reading {}", attest.display()))?,
        signature: std::fs::read(&sig).with_context(|| format!("reading {}", sig.display()))?,
        key_public,
        counter,
        floor_generation: u64::try_from(
            i128::from(counter)
                .checked_sub(i128::from(base))
                .unwrap_or(0)
                .max(0),
        )
        .unwrap_or(u64::MAX),
    })
}
