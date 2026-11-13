//! Making the rollback floor attestable: `TPM2_NV_Certify` signs the counter with a loaded key,
//! so a peer holding that key's public half can verify the floor without trusting anything the
//! machine says about itself. Carrying the statement to a peer, and binding the key to a device
//! certificate, are separate work.
//!
//! The key lives under the endorsement hierarchy and is `sign` + `restricted`: a restricted
//! signing key refuses externally supplied data, so its signature really does mean the TPM
//! produced the attestation itself.

use crate::argv;
use crate::exec::{Runner, require};
use crate::policy::{self, Workspace};
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct FloorAttestation {
    /// `TPMS_ATTEST` structure, marshalled.
    pub attestation: Vec<u8>,
    pub signature: Vec<u8>,
    /// Public area of the attestation key, so a verifier can check the signature and see that
    /// the key is restricted.
    pub key_public: Vec<u8>,
    pub counter: u64,
    pub floor_generation: u64,
}

/// Create (or recreate) the attestation key; returns its context path and public area.
///
/// Deterministic like the sealing parent: same owner-hierarchy seed and template, same key,
/// so nothing is persisted. Clearing the owner hierarchy changes the identity.
///
/// The parent is the owner primary, not the EK: the endorsement primary carries the standard
/// EK auth policy, so every operation on its child needs an endorsement-auth session. Binding
/// the key to a device certificate by credential activation belongs with the attestation
/// protocol.
///
/// `restricted` is not optional: a restricted signing key refuses externally supplied data
/// beginning with `TPM_GENERATED_VALUE`, so nobody holding it can sign a hand-written
/// structure that looks like a TPM attestation. The algorithm must name an explicit null
/// symmetric algorithm (`ecc256:ecdsa-sha256:null`); tpm2-tools otherwise fills in a default a
/// signing key cannot carry, which the TPM rejects as "unsupported symmetric algorithm".
///
/// # Errors
///
/// Any `tpm2_*` step fails, or the key's public area cannot be read back.
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
    // Reload from the blobs rather than reusing tpm2_create's context: on a TPM with no
    // resource manager the parent and the new key already fill the transient slots.
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

/// Sign the counter's current contents. `qualification` is the verifier's nonce: it lands in
/// the attestation's `extraData`, which stops a captured attestation from being replayed.
///
/// # Errors
///
/// The attestation key cannot be created or loaded, `tpm2_nvcertify` fails, the counter has
/// never been incremented, or the attestation or signature cannot be read back.
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
