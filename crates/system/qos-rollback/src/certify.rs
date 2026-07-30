//! Making the rollback floor attestable to someone else.
//!
//! `TPM2_NV_Certify` signs the contents of an NV index with a loaded key, so a
//! peer holding that key's public half can verify "this machine's rollback floor
//! is at generation N" without trusting anything the machine says about itself.
//!
//! This module deliberately stops at *producing* the signed statement. The
//! protocol that carries it to a peer, the trust decisions a peer makes about
//! the attestation key, and the binding of the key to a device certificate are a
//! separate concern with its own design; building half of it here would fix the
//! wrong shape in place.
//!
//! What it does guarantee is that the floor is exposed in a form that makes the
//! rest possible: a `TPMS_ATTEST` structure and a signature over it, with the
//! counter value and the NV index Name inside, plus a caller-supplied
//! qualifying nonce so a verifier can defeat replay.
//!
//! The attestation key is created under the endorsement hierarchy and is
//! restricted (`sign` + `restricted`), which is what makes a `TPM2_NV_Certify`
//! signature meaningful: a restricted signing key cannot be used to sign
//! arbitrary attacker-chosen data, so a valid signature really does mean the TPM
//! produced the attestation itself.

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
    /// Signature over it.
    pub signature: Vec<u8>,
    /// Attestation key's public area, so a verifier can check the signature and
    /// see that the key is restricted.
    pub key_public: Vec<u8>,
    /// The counter value the attestation covers, for convenience. The
    /// authoritative copy is inside `attestation`.
    pub counter: u64,
    /// Floor generation implied by that counter and the recorded base.
    pub floor_generation: u64,
}

/// Create (or recreate) the attestation key.
///
/// Deterministic in the same way the sealing parent is: same hierarchy seed,
/// same template, same key. So there is nothing to persist, and clearing the
/// endorsement hierarchy is the only thing that changes the identity — which is
/// the correct semantics for "this machine".
pub fn attestation_key(runner: &dyn Runner, ws: &Workspace) -> Result<(std::path::PathBuf, Vec<u8>)> {
    policy::flush(runner);
    let ek = ws.path("ek.ctx");
    require(
        runner,
        "tpm2_createprimary",
        &argv!["-C", "e", "-g", "sha256", "-G", "ecc", "-c", ek.as_os_str()],
        "creating the endorsement primary",
    )?;
    let akpub = ws.path("ak.pub");
    let akpriv = ws.path("ak.priv");
    let akctx = ws.path("ak.ctx");
    require(
        runner,
        "tpm2_create",
        &argv![
            "-C", ek.as_os_str(),
            "-G", "ecc",
            "-g", "sha256",
            "-u", akpub.as_os_str(),
            "-r", akpriv.as_os_str(),
            "-c", akctx.as_os_str(),
            "-a", "fixedtpm|fixedparent|sensitivedataorigin|userwithauth|restricted|sign",
        ],
        "creating the floor attestation key",
    )?;
    let pubbytes =
        std::fs::read(&akpub).with_context(|| format!("reading {}", akpub.display()))?;
    Ok((akctx, pubbytes))
}

/// Sign the counter's current contents.
///
/// `qualification` is the verifier's nonce; it lands in the attestation's
/// `extraData` and is what stops a captured attestation being replayed later.
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
            "-C", akctx.as_os_str(),
            "-c", &idx,
            "-g", "sha256",
            "-f", "plain",
            "-o", sig.as_os_str(),
            "--attestation", attest.as_os_str(),
            "--size", "8",
            "-q", &qual,
            &idx,
        ],
        "certifying the rollback floor counter",
    )?;

    let counter = crate::counter::read(runner, nv_index, scratch)?
        .context("the rollback floor counter has never been incremented, so there is nothing to certify")?;
    policy::flush(runner);
    Ok(FloorAttestation {
        attestation: std::fs::read(&attest)
            .with_context(|| format!("reading {}", attest.display()))?,
        signature: std::fs::read(&sig).with_context(|| format!("reading {}", sig.display()))?,
        key_public,
        counter,
        floor_generation: u64::try_from((i128::from(counter) - i128::from(base)).max(0))
            .unwrap_or(u64::MAX),
    })
}
