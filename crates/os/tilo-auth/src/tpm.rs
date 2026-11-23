//! Everything the system volume's unlock asks of the TPM, driven as the
//! tpm2-tools and systemd-creds programs the initramfs already carries,
//! through tilo-rollback's runner:
//!
//! * the sign-in wrapper: the user key as a sealed data object whose only
//!   policy is its authorisation value, with dictionary-attack protection on
//!   (no `noda`), so every wrong secret costs one of the TPM's tries;
//! * the attestation key a machine enrolled for unattended unlock quotes its
//!   boot chain with, so a sibling releases its half without a prompt;
//! * the sibling unlock's material, sealed to PCR 7 as a credential.

use crate::userkey::UserKey;
use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tilo_rollback::argv;
use tilo_rollback::exec::{Output, Runner, require};
use tilo_rollback::lockout;
use tilo_rollback::policy::{Workspace, flush};
use tilo_unlock_protocol::attest::{self, QUOTED_PCRS, Quote};
use zeroize::Zeroizing;

pub use crate::sibling::material::Attestation;

pub struct Sealed {
    pub public: Vec<u8>,
    pub private: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tries {
    pub left: u32,
    /// When none is left, the seconds until one comes back; while tries are
    /// left there is nothing to wait for.
    pub retry_in_s: Option<u32>,
}

pub enum Unseal {
    Key(UserKey),
    Wrong(Option<Tries>),
    /// The TPM refuses every try until its lockout decays.
    LockedOut(Option<Tries>),
}

/// The authorisation value reaches tpm2-tools through a file, never argv.
/// tpm2-tools reads a `file:` value as another authorisation string, so the
/// file holds the `hex:` form of the 32 bytes.
fn auth_file(ws: &Workspace, name: &str, auth: &[u8; 32]) -> Result<std::path::PathBuf> {
    let text = Zeroizing::new(format!("hex:{}", hex::encode(auth)));
    ws.put(name, text.as_bytes())
}

/// The standard storage key: ECC P-256, AES-128-CFB, restricted decrypt.
/// Deterministic, so every call makes the same key.
pub fn primary(runner: &dyn Runner, ws: &Workspace) -> Result<std::path::PathBuf> {
    flush(runner);
    let ctx = ws.path("signin-primary.ctx");
    require(
        runner,
        "tpm2_createprimary",
        &argv![
            "-C",
            "o",
            "-g",
            "sha256",
            "-G",
            "ecc256:aes128cfb",
            "-a",
            "fixedtpm|fixedparent|sensitivedataorigin|userwithauth|noda|restricted|decrypt",
            "-c",
            ctx.as_os_str()
        ],
        "creating the TPM storage key",
    )?;
    Ok(ctx)
}

/// Flush the transient objects the tools leave loaded, but not the session:
/// a TPM without a resource manager has three object slots.
fn flush_objects(runner: &dyn Runner) {
    let _ = runner.run("tpm2_flushcontext", &argv!["-t"], None);
}

/// An HMAC session salted to and bound to `parent`, encrypting the first
/// parameter of the command and of the response.
fn encrypted_session(
    runner: &dyn Runner,
    ws: &Workspace,
    parent: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let session = ws.path("signin-session.ctx");
    require(
        runner,
        "tpm2_startauthsession",
        &argv![
            "--hmac-session",
            "-c",
            parent.as_os_str(),
            "-S",
            session.as_os_str()
        ],
        "starting an encrypted TPM session",
    )?;
    // Starting it loads the parent once more; the command that follows loads
    // it again from its context.
    flush_objects(runner);
    Ok(session)
}

pub fn seal(runner: &dyn Runner, ws: &Workspace, key: &UserKey, auth: &[u8; 32]) -> Result<Sealed> {
    let parent = primary(runner, ws)?;
    flush_objects(runner);
    let session = match encrypted_session(runner, ws, &parent) {
        Ok(session) => session,
        Err(e) => {
            flush(runner);
            return Err(e);
        }
    };
    let parent_auth = format!("session:{}", session.display());
    let secret = ws.put("userkey.secret", key.expose())?;
    let auth_path = auth_file(ws, "userkey.auth", auth)?;
    let auth_spec = format!("file:{}", auth_path.display());
    let public = ws.path("userkey.pub");
    let private = ws.path("userkey.priv");
    let result = require(
        runner,
        "tpm2_create",
        &argv![
            "-C",
            parent.as_os_str(),
            "-P",
            &parent_auth,
            "-g",
            "sha256",
            "-u",
            public.as_os_str(),
            "-r",
            private.as_os_str(),
            "-i",
            secret.as_os_str(),
            "-p",
            &auth_spec,
            "-a",
            "fixedtpm|fixedparent|userwithauth",
        ],
        "sealing the user key",
    );
    let _ = std::fs::remove_file(&secret);
    let _ = std::fs::remove_file(&auth_path);
    flush(runner);
    result?;
    let sealed = Sealed {
        public: std::fs::read(&public).context("reading the sealed user key")?,
        private: std::fs::read(&private).context("reading the sealed user key")?,
    };
    let _ = std::fs::remove_file(&public);
    let _ = std::fs::remove_file(&private);
    Ok(sealed)
}

pub fn unseal(
    runner: &dyn Runner,
    ws: &Workspace,
    sealed: &Sealed,
    auth: &[u8; 32],
) -> Result<Unseal> {
    let parent = primary(runner, ws)?;
    let public = ws.put("unseal.pub", &sealed.public)?;
    let private = ws.put("unseal.priv", &sealed.private)?;
    let object = ws.path("unseal.ctx");
    let loaded = runner.run(
        "tpm2_load",
        &argv![
            "-C",
            parent.as_os_str(),
            "-u",
            public.as_os_str(),
            "-r",
            private.as_os_str(),
            "-c",
            object.as_os_str(),
        ],
        None,
    );
    // The object comes back from its saved context; with the parent still
    // loaded twice over, a TPM without a resource manager has no slot for it.
    flush(runner);
    let loaded = loaded?;
    if !loaded.ok() {
        bail!(
            "loading the sealed user key failed: {}",
            loaded.last_error_line()
        );
    }
    let session = match encrypted_session(runner, ws, &parent) {
        Ok(session) => session,
        Err(e) => {
            flush(runner);
            return Err(e);
        }
    };
    let auth_path = auth_file(ws, "unseal.auth", auth)?;
    let auth_spec = format!("session:{}+file:{}", session.display(), auth_path.display());
    let output = ws.path("unseal.out");
    let result = runner.run(
        "tpm2_unseal",
        &argv![
            "-c",
            object.as_os_str(),
            "-p",
            &auth_spec,
            "-o",
            output.as_os_str()
        ],
        None,
    );
    let _ = std::fs::remove_file(&auth_path);
    flush(runner);
    let out = result?;
    if out.ok() {
        let bytes = Zeroizing::new(std::fs::read(&output).context("reading the unsealed key")?);
        let _ = std::fs::remove_file(&output);
        return UserKey::from_slice(&bytes)
            .map(Unseal::Key)
            .context("the sealed object does not hold a user key");
    }
    match classify(&out) {
        Failure::Wrong => Ok(Unseal::Wrong(tries(runner))),
        Failure::LockedOut => Ok(Unseal::LockedOut(tries(runner))),
        Failure::Other => bail!("unsealing the user key failed: {}", out.last_error_line()),
    }
}

enum Failure {
    Wrong,
    LockedOut,
    Other,
}

/// `TPM_RC_LOCKOUT`, a warning: the TPM refuses every DA-protected
/// authorisation until its lockout decays.
const TPM_RC_LOCKOUT: u32 = 0x921;
const TPM_RC_AUTH_FAIL: u32 = 0x08E;
const TPM_RC_BAD_AUTH: u32 = 0x0A2;

/// By the TPM's response code, which tpm2-tools prints as
/// `Esys_Unseal(0x98E)`, never by the words around it.
fn classify(out: &Output) -> Failure {
    match response_code(&out.stderr).map(base_code) {
        Some(TPM_RC_LOCKOUT) => Failure::LockedOut,
        Some(TPM_RC_AUTH_FAIL | TPM_RC_BAD_AUTH) => Failure::Wrong,
        _ => Failure::Other,
    }
}

fn response_code(stderr: &str) -> Option<u32> {
    stderr.lines().rev().find_map(|line| {
        let start = line.find("Esys_")?;
        let rest = line.get(start..)?;
        let code = rest.split_once("(0x")?.1.split_once(')')?.0;
        u32::from_str_radix(code, 16).ok()
    })
}

/// The code without the TSS layer and, for a format-one code, without the
/// handle, session or parameter it names.
const fn base_code(code: u32) -> u32 {
    let code = code & 0xFFFF;
    if code & 0x080 != 0 {
        code & 0x0BF
    } else {
        code
    }
}

pub fn tries(runner: &dyn Runner) -> Option<Tries> {
    let params = lockout::read(runner).ok()?;
    if params.max_tries == 0 {
        return None;
    }
    let left = if params.in_lockout {
        0
    } else {
        params.max_tries.saturating_sub(params.counter)
    };
    Some(Tries {
        left,
        retry_in_s: (left == 0).then_some(params.recovery_time),
    })
}

pub fn usable(runner: &dyn Runner) -> bool {
    lockout::read(runner).is_ok_and(|p| p.max_tries > 0 && p.recovery_time > 0)
}

/// Where Linux exposes the TPM with the kernel's resource manager.
pub const TPM_DEVICE: &str = "/dev/tpmrm0";

/// Whether this machine has a TPM to ask: one named by a TCTI, or the
/// kernel's. Without one a machine can still enrol for sibling unlock; it
/// just cannot be released without a prompt.
pub fn present(tcti: Option<&str>) -> bool {
    tcti.is_some() || Path::new(TPM_DEVICE).exists()
}

/// PCR bank and indices a quote covers: the loaded images and the Secure
/// Boot policy.
const PCR_LIST: &str = "sha256:4,7";

/// The owner-hierarchy primary the attestation key lives under: the
/// tpm2-tools default ECC template, which an enrolled key's blobs are bound
/// to. Its seed and template make it the same object on every boot.
fn attestation_parent(runner: &dyn Runner, ws: &Workspace) -> Result<std::path::PathBuf> {
    flush(runner);
    let ctx = ws.path("attestation-parent.ctx");
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
            ctx.as_os_str()
        ],
        "creating the attestation key's parent",
    )?;
    Ok(ctx)
}

/// Load the attestation key from its blobs and return its saved context;
/// every transient object is flushed again, so the next command reloads it
/// on a TPM without a resource manager.
fn load_attestation(
    runner: &dyn Runner,
    ws: &Workspace,
    public: &Path,
    private: &Path,
) -> Result<std::path::PathBuf> {
    let parent = attestation_parent(runner, ws)?;
    let ctx = ws.path("ak.ctx");
    let loaded = require(
        runner,
        "tpm2_load",
        &argv![
            "-C",
            parent.as_os_str(),
            "-u",
            public.as_os_str(),
            "-r",
            private.as_os_str(),
            "-c",
            ctx.as_os_str()
        ],
        "loading the attestation key",
    );
    flush(runner);
    loaded?;
    Ok(ctx)
}

/// Create the attestation key under this machine's TPM and return its blobs
/// and SEC1 point.
///
/// # Errors
///
/// Any `tpm2_*` step fails, or the key's public area cannot be read back.
pub fn create_attestation_key(runner: &dyn Runner) -> Result<(Attestation, Vec<u8>)> {
    let dir = crate::scratch()?;
    let ws = Workspace::new(dir.path());
    let public = ws.path("ak.pub");
    let private = ws.path("ak.priv");
    let parent = attestation_parent(runner, &ws)?;
    let created = require(
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
            public.as_os_str(),
            "-r",
            private.as_os_str(),
            "-a",
            "fixedtpm|fixedparent|sensitivedataorigin|userwithauth|restricted|sign"
        ],
        "creating the attestation key",
    );
    flush(runner);
    created?;
    let ctx = load_attestation(runner, &ws, &public, &private)?;
    let area = ws.path("ak.tss");
    let read = require(
        runner,
        "tpm2_readpublic",
        &argv!["-c", ctx.as_os_str(), "-f", "tss", "-o", area.as_os_str()],
        "reading the attestation key back",
    );
    flush(runner);
    read?;
    let area = std::fs::read(&area).context("the attestation key's public area")?;
    let point = attest::sec1_from_tpm_public(&area).map_err(|error| anyhow!("{error}"))?;
    let blobs = Attestation {
        public: hex::encode(std::fs::read(&public).context("the attestation key's public half")?),
        private: hex::encode(
            std::fs::read(&private).context("the attestation key's private half")?,
        ),
    };
    Ok((blobs, point))
}

/// Quote PCR 4 and 7 for one request, with `qualifying` as the attestation's
/// `extraData`: a recorded quote answers no other challenge.
///
/// # Errors
///
/// The key cannot be loaded, or the TPM cannot quote. The caller treats this
/// as "no attestation" and lets the owner decide.
pub fn quote(runner: &dyn Runner, key: &Attestation, qualifying: &[u8; 32]) -> Result<Quote> {
    let (public, private) = key.blobs()?;
    let dir = crate::scratch()?;
    let ws = Workspace::new(dir.path());
    let public = ws.put("ak.pub", &public)?;
    let private = ws.put("ak.priv", &private)?;
    let ctx = load_attestation(runner, &ws, &public, &private)?;
    let attestation = ws.path("attest");
    let signature = ws.path("sig");
    let values = ws.path("pcr");
    let qualifying = hex::encode(qualifying);
    let quoted = require(
        runner,
        "tpm2_quote",
        &argv![
            "-c",
            ctx.as_os_str(),
            "-l",
            PCR_LIST,
            "-q",
            &qualifying,
            "-m",
            attestation.as_os_str(),
            "-s",
            signature.as_os_str(),
            "-o",
            values.as_os_str(),
            "-F",
            "values",
            "-g",
            "sha256"
        ],
        "quoting the boot chain",
    );
    flush(runner);
    quoted?;
    Ok(Quote {
        attest: std::fs::read(&attestation).context("the quote")?,
        signature: attest::signature_from_tpm(
            &std::fs::read(&signature).context("the quote's signature")?,
        )
        .map_err(|error| anyhow!("{error}"))?
        .to_vec(),
        pcrs: attest::pcr_values(
            &std::fs::read(&values).context("the quoted PCRs")?,
            &QUOTED_PCRS,
        )
        .map_err(|error| anyhow!("{error}"))?,
    })
}

/// The PCR 4 + 7 composite this machine measures now, which is what an
/// enrolment offers as the boot chain to accept.
///
/// # Errors
///
/// The TPM cannot read the PCRs.
pub fn current_chain(runner: &dyn Runner) -> Result<[u8; 32]> {
    let dir = crate::scratch()?;
    let values = dir.path().join("pcr");
    require(
        runner,
        "tpm2_pcrread",
        &argv![PCR_LIST, "-o", values.as_os_str(), "-F", "values"],
        "reading the boot chain",
    )?;
    let bytes = std::fs::read(&values).context("the measured PCRs")?;
    let pcrs = attest::pcr_values(&bytes, &QUOTED_PCRS).map_err(|error| anyhow!("{error}"))?;
    Ok(attest::pcr_composite(&pcrs))
}

/// The credential name the sibling unlock's material is sealed under.
const CREDENTIAL: &str = "tilo-sibling-unlock";

/// How the sibling unlock's material is kept in the LUKS2 header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sealing {
    /// A systemd-creds credential sealed to this TPM and PCR 7.
    Tpm2,
    /// In the clear, for a machine without a TPM.
    Cleartext,
}

impl Sealing {
    /// The strongest sealing this machine offers.
    pub fn available(tcti: Option<&str>) -> Self {
        if present(tcti) {
            Self::Tpm2
        } else {
            Self::Cleartext
        }
    }
}

fn creds(
    runner: &dyn Runner,
    args: &[&std::ffi::OsStr],
    input: &[u8],
    what: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    let out = runner.run("systemd-creds", args, Some(input))?;
    if !out.ok() {
        bail!(
            "{what} failed (systemd-creds exit {}): {}",
            out.code,
            out.last_error_line()
        );
    }
    Ok(Zeroizing::new(out.stdout))
}

/// Seal `plaintext` as [`Sealing`] says, base64 for the header.
///
/// # Errors
///
/// The TPM cannot seal it.
pub fn seal_material(runner: &dyn Runner, sealing: Sealing, plaintext: &[u8]) -> Result<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    match sealing {
        Sealing::Cleartext => Ok(engine.encode(plaintext)),
        Sealing::Tpm2 => {
            let name = format!("--name={CREDENTIAL}");
            let blob = creds(
                runner,
                &argv![
                    "encrypt",
                    "--with-key=tpm2",
                    "--tpm2-device=auto",
                    "--tpm2-pcrs=7",
                    &name,
                    "-",
                    "-"
                ],
                plaintext,
                "sealing the unlock material to the TPM",
            )?;
            Ok(engine.encode(blob.as_slice()))
        }
    }
}

/// Opens what [`seal_material`] produced.
///
/// # Errors
///
/// The blob is not base64, or the TPM refuses to unseal it (another machine,
/// or a changed Secure Boot state).
pub fn unseal_material(
    runner: &dyn Runner,
    sealing: Sealing,
    stored: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    let bytes = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(stored)
            .context("the stored unlock material is not base64")?,
    );
    match sealing {
        Sealing::Cleartext => Ok(bytes),
        Sealing::Tpm2 => {
            let name = format!("--name={CREDENTIAL}");
            creds(
                runner,
                &argv!["decrypt", &name, "-", "-"],
                &bytes,
                "unsealing the unlock material",
            )
        }
    }
}
