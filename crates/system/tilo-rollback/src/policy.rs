//! Building and satisfying the unseal policy.
//!
//! The policy is a three-part `AND`, applied in this order — order matters because a policy
//! digest is a running hash and `TPM2_PolicyAuthorize` *resets* it:
//!
//! 1. `TPM2_PolicyPCR` over the signed prediction's PCR set — in a policy session this reads
//!    the *live* PCRs, so it only produces the expected digest when the machine really is in
//!    the predicted state.
//! 2. `TPM2_PolicyAuthorize` with the prediction authority's public key and
//!    `policyRef = generation`: checks step 1 produced exactly the digest the authority
//!    signed, then replaces the session digest with a value derived only from the key name
//!    and policy reference — what lets a firmware update ship a new signature instead of
//!    forcing a re-seal, and why the generation must be in `policyRef` (see crate-level notes).
//! 3. `TPM2_PolicyNV`, asserting `counter <= base + generation`, i.e. the rollback floor is
//!    at or below this generation.
//! 4. `TPM2_PolicyAuthValue` when a PIN is in use, so it's checked by the TPM as the sealed
//!    object's authorisation value and covered by the dictionary-attack lockout (the sealed
//!    object must *not* carry `noda` — see [`seal`](crate::keyslots::seal)).
//!
//! # Signing side
//!
//! The authority signs `SHA256(approvedPolicy || policyRef)` with the PCR prediction key.
//! `approvedPolicy` is the digest a trial session reaches after `TPM2_PolicyPCR` over the
//! *predicted* values, so prediction and signature can both be produced offline with no TPM
//! involvement beyond a trial session.
//!
//! A token may carry several predictions ([`Prediction`] list); unlock tries each in turn, so
//! one token can cover before *and* after a firmware update, removing the window where an
//! update has been applied but not yet rebooted into.

use crate::argv;
use crate::exec::{Runner, require, trace_failure};
use crate::generation;
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// One signed statement of "these are the measurements to expect".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prediction {
    /// Policy digest a trial `TPM2_PolicyPCR` reaches over the predicted values.
    pub approved_policy: Vec<u8>,
    /// `SHA256(approved_policy || policy_ref)` signed by the prediction key.
    pub signature: Vec<u8>,
    /// Free-text note (`"as built"`, `"post-fwupd"`) for diagnostics only.
    pub label: String,
}

/// Wrap a raw PKCS#1 v1.5 signature in the `TPMT_SIGNATURE` structure the TPM expects.
///
/// `TPM2_VerifySignature` mints the ticket `TPM2_PolicyAuthorize` needs, but only if the
/// signature is handed over as a TPM structure — `tpm2_verifysignature --format=rsassa` with
/// bare external-tool output instead verifies in software and writes no ticket, surfacing one
/// step later as a missing file rather than "the TPM was never asked".
///
/// Layout: `sigAlg` = `TPM_ALG_RSASSA` (0x0014), `hash` = `TPM_ALG_SHA256` (0x000B), then a
/// `TPM2B_PUBLIC_KEY_RSA` (big-endian size, then the bytes).
///
/// # Errors
///
/// Returns an error if `signature` is too long to fit the `TPM2B_PUBLIC_KEY_RSA` size field.
pub fn marshal_rsassa_sha256(signature: &[u8]) -> Result<Vec<u8>> {
    let len = u16::try_from(signature.len()).with_context(|| {
        format!(
            "a {}-byte signature does not fit TPM2B_PUBLIC_KEY_RSA",
            signature.len()
        )
    })?;
    // Just a capacity hint: `len` above already proves `signature.len()` fits a u16, so this
    // cannot meaningfully overflow, and a wrong hint would only cost a reallocation.
    let mut out = Vec::with_capacity(signature.len().saturating_add(6));
    out.extend_from_slice(&0x0014u16.to_be_bytes()); // TPM_ALG_RSASSA
    out.extend_from_slice(&0x000Bu16.to_be_bytes()); // TPM_ALG_SHA256
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(signature);
    Ok(out)
}

/// PCR selection in the form tpm2-tools wants: `sha256:7,11`.
pub fn pcr_selection(bank: &str, pcrs: &[u32]) -> String {
    let list = pcrs
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("{bank}:{list}")
}

/// Where the image ships the signed predictions it was built with.
pub const DEFAULT_PREDICTION_DIR: &str = "/usr/share/tilo/pcr-predictions";

/// Read the signed predictions for one generation out of a directory. A device cannot
/// produce these itself: signing needs the private prediction key, deliberately not on the
/// device — a machine that could sign its own predictions could authorise any measurement
/// state it liked, defeating the whole point of `PolicyAuthorize`.
///
/// # Errors
///
/// Returns an error if `dir` cannot be read, if any `.json` file in it fails to read or
/// parse, or if one for the requested `generation` is missing its `approved_policy` or
/// `signature` field or either is not valid base64.
pub fn load_signed_predictions(dir: &Path, generation: u64) -> Result<Vec<Prediction>> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading the prediction directory {}", dir.display()))?;
    let mut names: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for p in names {
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        if v.get("generation").and_then(serde_json::Value::as_u64) != Some(generation) {
            continue;
        }
        out.push(Prediction {
            approved_policy: b64
                .decode(
                    v.get("approved_policy")
                        .and_then(serde_json::Value::as_str)
                        .context("approved_policy")?,
                )
                .with_context(|| format!("{}: approved_policy is not base64", p.display()))?,
            signature: b64
                .decode(
                    v.get("signature")
                        .and_then(serde_json::Value::as_str)
                        .context("signature")?,
                )
                .with_context(|| format!("{}: signature is not base64", p.display()))?,
            label: v
                .get("label")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    Ok(out)
}

/// Write a signed prediction where [`load_signed_predictions`] will find it.
///
/// # Errors
///
/// Returns an error if `dir` cannot be created, or if the prediction file cannot be
/// serialised or written.
pub fn write_signed_prediction(
    dir: &Path,
    generation: u64,
    bank: &str,
    pcrs: &[u32],
    prediction: &Prediction,
) -> Result<PathBuf> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let doc = serde_json::json!({
        "generation": generation,
        "pcr_bank": bank,
        "pcrs": pcrs,
        "approved_policy": b64.encode(&prediction.approved_policy),
        "signature": b64.encode(&prediction.signature),
        "label": prediction.label,
    });
    let name = if prediction.label.is_empty() {
        format!("generation-{generation}.json")
    } else {
        format!(
            "generation-{generation}-{}.json",
            prediction.label.replace(['/', ' ', '\\'], "-")
        )
    };
    let path = dir.join(name);
    let json = serde_json::to_string_pretty(&doc)?;
    tilo_files::native::atomic::write_with_mode(&path, json.as_bytes(), 0o644)
        .with_context(|| format!("atomically writing {}", path.display()))?;
    Ok(path)
}

/// Scratch directory plus the handful of files the tpm2 tools pass by path.
pub struct Workspace {
    pub dir: PathBuf,
}

impl Workspace {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    /// Write bytes to a scratch file and return the path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn put(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let p = self.path(name);
        std::fs::write(&p, bytes).with_context(|| format!("writing {}", p.display()))?;
        Ok(p)
    }
}

/// Flush every transient object, loaded session and saved session. A bare `swtpm` socket has
/// no resource manager and runs out of object slots after three loads (`/dev/tpmrm0` on real
/// hardware does not); flushing between steps keeps behaviour the same either way. Failures
/// are ignored on purpose — often there's nothing to flush.
pub fn flush(runner: &dyn Runner) {
    for flag in ["-t", "-l", "-s"] {
        let _ = runner.run("tpm2_flushcontext", &argv![flag], None);
    }
}

/// Load the prediction authority's public key and return its TPM Name — that, not the PEM,
/// is what `TPM2_PolicyAuthorize` commits to and what a sealed token is bound to.
///
/// # Errors
///
/// Returns an error if loading the key or reading its public area fails.
pub fn key_name(runner: &dyn Runner, ws: &Workspace, public_key_pem: &Path) -> Result<Vec<u8>> {
    flush(runner);
    let ctx = ws.path("signkey.ctx");
    require(
        runner,
        "tpm2_loadexternal",
        &argv![
            "-C",
            "o",
            "-G",
            "rsa",
            "-u",
            public_key_pem.as_os_str(),
            "-c",
            ctx.as_os_str(),
        ],
        "loading the PCR prediction public key",
    )?;
    let name = ws.path("signkey.name");
    require(
        runner,
        "tpm2_readpublic",
        &argv!["-c", ctx.as_os_str(), "-n", name.as_os_str()],
        "reading the PCR prediction key name",
    )?;
    let bytes = std::fs::read(&name).with_context(|| format!("reading {}", name.display()))?;
    flush(runner);
    Ok(bytes)
}

/// Compute the digest a `TPM2_PolicyPCR` over `pcr_values` reaches. `pcr_values` is a
/// `tpm2_pcrread -o` blob (the predicted measurements); pass `None` to use the machine's live
/// PCRs, as enrollment does when generating its own first prediction on the machine itself.
///
/// # Errors
///
/// Returns an error if the trial policy session or reading its digest fails.
pub fn approved_policy(
    runner: &dyn Runner,
    ws: &Workspace,
    selection: &str,
    pcr_values: Option<&Path>,
) -> Result<Vec<u8>> {
    flush(runner);
    let sess = ws.path("trial_pcr.ctx");
    require(
        runner,
        "tpm2_startauthsession",
        &argv!["-S", sess.as_os_str()],
        "starting a trial policy session",
    )?;
    let mut args = argv!["-S", sess.as_os_str(), "-l", selection];
    if let Some(p) = pcr_values {
        args.push(OsStr::new("-f"));
        args.push(p.as_os_str());
    }
    let r = require(
        runner,
        "tpm2_policypcr",
        &args,
        "computing the PCR policy digest",
    );
    let out = ws.path("approved.bin");
    let digest = match r {
        Ok(_) => {
            require(
                runner,
                "tpm2_getpolicydigest",
                &argv!["-S", sess.as_os_str(), "-o", out.as_os_str()],
                "reading the trial session's policy digest",
            )?;
            std::fs::read(&out).with_context(|| format!("reading {}", out.display()))
        }
        Err(e) => Err(e),
    };
    let _ = runner.run("tpm2_flushcontext", &argv![sess.as_os_str()], None);
    digest
}

/// The NV counter comparison embedded in a sealed generation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvConstraint {
    /// TPM NV index containing the monotonic rollback floor.
    pub index: u32,
    /// Absolute counter value this generation permits or exceeds.
    pub operand: u64,
}

/// Everything one generation's seal policy is built from: what sealing commits to, what a
/// token records, and what unlocking replays.
#[derive(Debug, Clone, Copy)]
pub struct GenerationPolicy<'a> {
    /// The generation, which is also the `PolicyAuthorize` policy reference.
    pub generation: u64,
    /// TPM Name of the prediction authority's public key.
    pub key_name: &'a [u8],
    pub pcr_bank: &'a str,
    /// PCRs covered by the signed prediction, before `PolicyAuthorize`.
    pub pcrs: &'a [u32],
    /// Device-local PCRs applied after `PolicyAuthorize`.
    pub platform_pcrs: &'a [u32],
    pub nv: NvConstraint,
    /// Is a PIN (`PolicyAuthValue`) part of the policy?
    pub pin: bool,
}

impl GenerationPolicy<'_> {
    fn selection(&self) -> String {
        pcr_selection(self.pcr_bank, self.pcrs)
    }

    fn platform_selection(&self) -> String {
        pcr_selection(self.pcr_bank, self.platform_pcrs)
    }
}

/// The full seal policy digest for one generation.
///
/// # Errors
///
/// Returns an error if any step of the trial policy session fails.
pub fn seal_policy(
    runner: &dyn Runner,
    ws: &Workspace,
    policy: &GenerationPolicy<'_>,
) -> Result<Vec<u8>> {
    flush(runner);
    let name_file = ws.put("signkey.name", policy.key_name)?;
    let ref_file = ws.put("policyref.bin", &generation::policy_ref(policy.generation))?;
    let operand_file = ws.put("operand.bin", &policy.nv.operand.to_be_bytes())?;
    let idx = format!("0x{:08X}", policy.nv.index);
    let platform_selection = policy.platform_selection();
    let sess = ws.path("trial_seal.ctx");

    require(
        runner,
        "tpm2_startauthsession",
        &argv!["-S", sess.as_os_str()],
        "starting a trial policy session",
    )?;
    let build = || -> Result<Vec<u8>> {
        // PolicyAuthorize replaces whatever digest precedes it, so in a trial session the resulting
        // digest depends only on the key name and policy reference — what makes predictions
        // replaceable without re-sealing.
        require(
            runner,
            "tpm2_policyauthorize",
            &argv![
                "-S",
                sess.as_os_str(),
                "-n",
                name_file.as_os_str(),
                "-q",
                ref_file.as_os_str(),
            ],
            "computing the PolicyAuthorize digest",
        )?;
        require(
            runner,
            "tpm2_policypcr",
            &argv!["-S", sess.as_os_str(), "-l", &platform_selection],
            "binding the seal to this machine's platform PCRs",
        )?;
        require(
            runner,
            "tpm2_policynv",
            &argv![
                "-S",
                sess.as_os_str(),
                "-i",
                operand_file.as_os_str(),
                &idx,
                "ule",
                "-C",
                &idx,
            ],
            "computing the rollback-floor PolicyNV digest",
        )?;
        if policy.pin {
            require(
                runner,
                "tpm2_policyauthvalue",
                &argv!["-S", sess.as_os_str()],
                "computing the PIN PolicyAuthValue digest",
            )?;
        }
        let out = ws.path("sealpolicy.bin");
        require(
            runner,
            "tpm2_getpolicydigest",
            &argv!["-S", sess.as_os_str(), "-o", out.as_os_str()],
            "reading the seal policy digest",
        )?;
        std::fs::read(&out).with_context(|| format!("reading {}", out.display()))
    };
    let result = build();
    let _ = runner.run("tpm2_flushcontext", &argv![sess.as_os_str()], None);
    result
}

/// Sign a prediction. Needs the *private* key, so this runs on the build or update
/// authority, never on the device.
///
/// # Errors
///
/// Returns an error if the private key cannot be read or parsed, or signing fails.
pub fn sign_prediction(
    _runner: &dyn Runner,
    _ws: &Workspace,
    private_key_pem: &Path,
    approved_policy: &[u8],
    gen_num: u64,
) -> Result<Vec<u8>> {
    let mut message = approved_policy.to_vec();
    message.extend_from_slice(&generation::policy_ref(gen_num));
    let key = Zeroizing::new(
        std::fs::read_to_string(private_key_pem)
            .with_context(|| format!("reading {}", private_key_pem.display()))?,
    );
    tilo_crypto::sign_rsa_sha256(&key, &message).map_err(anyhow::Error::msg)
}

/// A policy session that has been driven to the seal policy, ready to authorise
/// an unseal.
pub struct SatisfiedSession {
    pub context: PathBuf,
}

/// The verification ticket and its approved-policy file, once a prediction's signature has
/// been confirmed — everything [`drive_policy_session`] needs from stage 1.
struct PredictionTicket {
    approved: PathBuf,
    ticket: PathBuf,
}

/// Drive a real policy session through the whole chain.
///
/// Returns `Ok(None)` when the *prediction* did not apply (wrong state, wrong signer, or
/// wrong policy reference) — an expected outcome when trying several predictions, not an
/// error. `Err` is reserved for "the mechanism itself is broken", except a `PolicyNV` failure
/// which *is* an error: it means the machine is below the rollback floor.
///
/// # Errors
///
/// Returns an error if loading the prediction key or minting its verification ticket fails
/// outright (as opposed to simply not verifying), if the policy session fails to start, if
/// reading live PCRs fails, or if the rollback-floor `PolicyNV` step fails.
pub fn satisfy(
    runner: &dyn Runner,
    ws: &Workspace,
    public_key_pem: &Path,
    policy: &GenerationPolicy<'_>,
    prediction: &Prediction,
) -> Result<Option<SatisfiedSession>> {
    let Some(prediction_ticket) =
        verify_prediction_signature(runner, ws, public_key_pem, prediction, policy.generation)?
    else {
        return Ok(None);
    };
    drive_policy_session(runner, ws, policy, &prediction_ticket)
}

/// Stage 1 of [`satisfy`]: mint the verification ticket with only the external key loaded.
/// Keeping the sealed object out until the ticket exists is what lets this run on a 3-slot
/// TPM.
///
/// Returns `Ok(None)` if the prediction is not signed by this key, or not over this
/// generation's policy reference.
fn verify_prediction_signature(
    runner: &dyn Runner,
    ws: &Workspace,
    public_key_pem: &Path,
    prediction: &Prediction,
    gen_num: u64,
) -> Result<Option<PredictionTicket>> {
    flush(runner);
    let sk = ws.path("verifykey.ctx");
    // `-C o` is load-bearing: `tpm2_loadexternal` defaults to the NULL hierarchy, and the TPM
    // issues no validation ticket for a NULL-hierarchy key. Without it, `tpm2_verifysignature`
    // succeeds silently with no ticket, surfacing later as `TPM2_PolicyAuthorize` missing a file.
    // The key's Name (what the policy is bound to) doesn't depend on the hierarchy, so this
    // changes nothing about what a sealed token accepts.
    require(
        runner,
        "tpm2_loadexternal",
        &argv![
            "-C",
            "o",
            "-G",
            "rsa",
            "-u",
            public_key_pem.as_os_str(),
            "-c",
            sk.as_os_str(),
        ],
        "loading the PCR prediction public key",
    )?;
    let approved = ws.put("approved.bin", &prediction.approved_policy)?;
    let mut message = prediction.approved_policy.clone();
    message.extend_from_slice(&generation::policy_ref(gen_num));
    let msg = ws.put("verify_msg.bin", &message)?;
    let sig = ws.put(
        "prediction.sig",
        &marshal_rsassa_sha256(&prediction.signature)?,
    )?;
    let ticket = ws.path("ticket.bin");
    let verify_args = argv![
        "-c",
        sk.as_os_str(),
        "-g",
        "sha256",
        "-m",
        msg.as_os_str(),
        "-s",
        sig.as_os_str(),
        "-t",
        ticket.as_os_str(),
    ];
    let vs = runner.run("tpm2_verifysignature", &verify_args, None)?;
    let _ = runner.run("tpm2_flushcontext", &argv!["-t"], None);
    if !vs.ok() {
        // Not signed by this key, or not over this generation's policy ref.
        trace_failure("tpm2_verifysignature", &verify_args, &vs);
        return Ok(None);
    }
    Ok(Some(PredictionTicket { approved, ticket }))
}

/// Stage 2 of [`satisfy`]: the real policy session, spending the ticket stage 1 minted.
fn drive_policy_session(
    runner: &dyn Runner,
    ws: &Workspace,
    policy: &GenerationPolicy<'_>,
    prediction_ticket: &PredictionTicket,
) -> Result<Option<SatisfiedSession>> {
    let gen_num = policy.generation;
    let selection = policy.selection();
    let platform_selection = policy.platform_selection();
    let sess = ws.path("policy.ctx");
    require(
        runner,
        "tpm2_startauthsession",
        &argv!["--policy-session", "-S", sess.as_os_str()],
        "starting the unseal policy session",
    )?;
    let name_file = ws.put("signkey.name", policy.key_name)?;
    let ref_file = ws.put("policyref.bin", &generation::policy_ref(gen_num))?;
    let operand_file = ws.put("operand.bin", &policy.nv.operand.to_be_bytes())?;
    let idx = format!("0x{:08X}", policy.nv.index);

    let drive = || -> Result<Option<()>> {
        // Live PCRs.
        let pcr = runner.run(
            "tpm2_policypcr",
            &argv!["-S", sess.as_os_str(), "-l", &selection],
            None,
        )?;
        if !pcr.ok() {
            bail!(
                "reading live PCRs for the policy session failed: {}",
                pcr.last_error_line()
            );
        }
        // Does the live state match what was signed?
        let auth_args = argv![
            "-S",
            sess.as_os_str(),
            "-i",
            prediction_ticket.approved.as_os_str(),
            "-n",
            name_file.as_os_str(),
            "-q",
            ref_file.as_os_str(),
            "-t",
            prediction_ticket.ticket.as_os_str(),
        ];
        let auth = runner.run("tpm2_policyauthorize", &auth_args, None)?;
        if !auth.ok() {
            trace_failure("tpm2_policyauthorize", &auth_args, &auth);
            return Ok(None); // wrong prediction for the current state
        }
        let platform = runner.run(
            "tpm2_policypcr",
            &argv!["-S", sess.as_os_str(), "-l", &platform_selection],
            None,
        )?;
        if !platform.ok() {
            bail!(
                "reading device-local platform PCRs failed: {}",
                platform.last_error_line()
            );
        }
        // Rollback floor.
        let nv = runner.run(
            "tpm2_policynv",
            &argv![
                "-S",
                sess.as_os_str(),
                "-i",
                operand_file.as_os_str(),
                &idx,
                "ule",
                "-C",
                &idx,
            ],
            None,
        )?;
        if !nv.ok() {
            bail!(
                "the rollback floor refuses generation {gen_num}: this image is older than the oldest \
                 generation this machine still permits ({})",
                nv.last_error_line()
            );
        }
        if policy.pin {
            require(
                runner,
                "tpm2_policyauthvalue",
                &argv!["-S", sess.as_os_str()],
                "adding the PIN authorisation to the policy session",
            )?;
        }
        Ok(Some(()))
    };

    match drive() {
        Ok(Some(())) => Ok(Some(SatisfiedSession { context: sess })),
        Ok(None) => {
            let _ = runner.run("tpm2_flushcontext", &argv![sess.as_os_str()], None);
            Ok(None)
        }
        Err(e) => {
            let _ = runner.run("tpm2_flushcontext", &argv![sess.as_os_str()], None);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_marshalling_matches_tool_and_tpm_layouts() {
        // Phase `pcr_selection_matches_the_tools_syntax`.
        {
            assert_eq!(pcr_selection("sha256", &[7, 11]), "sha256:7,11");
            assert_eq!(pcr_selection("sha384", &[0]), "sha384:0");
        }
        // Phase `rsassa_marshalling_has_the_tpm_structure_layout`.
        {
            let sig = vec![0xABu8; 256];
            let m = marshal_rsassa_sha256(&sig).unwrap();
            assert_eq!(m.len(), 256 + 6);
            assert_eq!(&m[0..2], &[0x00, 0x14], "sigAlg must be TPM_ALG_RSASSA");
            assert_eq!(&m[2..4], &[0x00, 0x0B], "hash must be TPM_ALG_SHA256");
            assert_eq!(&m[4..6], &[0x01, 0x00], "size must be 256 big-endian");
            assert_eq!(&m[6..], &sig[..]);
        }
        // Phase `an_oversized_signature_is_refused_rather_than_truncated`.
        {
            assert!(marshal_rsassa_sha256(&vec![0u8; 70_000]).is_err());
        }
        // Phase `workspace_writes_scratch_files`.
        {
            let d = tempfile::tempdir().unwrap();
            let ws = Workspace::new(d.path());
            let p = ws.put("x.bin", b"abc").unwrap();
            assert_eq!(std::fs::read(p).unwrap(), b"abc");
        }
    }
}
