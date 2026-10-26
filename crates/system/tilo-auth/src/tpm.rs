//! The TPM + secret wrapper: the user key as a sealed data object whose only
//! policy is its authorisation value, with dictionary-attack protection on
//! (no `noda`), so every wrong secret costs one of the TPM's tries.
//!
//! The object lives under a primary made fresh from the standard ECC P-256
//! storage key template on the owner hierarchy each time, so nothing is
//! persisted in the TPM and a Windows-owned persistent SRK is never touched.
//! The template has `noda`, as the standard one does: only the person's own
//! object counts tries, so a locked-out TPM still loads it and can say so, and
//! a new secret can be sealed while the lockout lasts.
//!
//! Sealing and unsealing run in an HMAC session salted to that primary with
//! parameter encryption both ways, so neither the authorisation value nor the
//! user key crosses the bus to a discrete TPM in the clear: the value only
//! keys the session's HMAC, and the key and sealed data travel encrypted.

use crate::userkey::UserKey;
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use tilo_rollback::argv;
use tilo_rollback::exec::{Output, Runner, require};
use tilo_rollback::lockout;
use tilo_rollback::policy::{Workspace, flush};
use zeroize::Zeroizing;

pub struct Sealed {
    pub public: Vec<u8>,
    pub private: Vec<u8>,
}

/// What the TPM's lockout allows right now, for the screen to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tries {
    /// Wrong secrets left before the TPM makes everyone wait.
    pub left: u32,
    /// When none is left, the seconds until one comes back; while tries are
    /// left there is nothing to wait for.
    pub retry_in_s: Option<u32>,
}

pub enum Unseal {
    Key(UserKey),
    /// The secret was wrong and cost a try.
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
/// `TPM_RC_AUTH_FAIL`: a wrong value that incremented the counter.
const TPM_RC_AUTH_FAIL: u32 = 0x08E;
/// `TPM_RC_BAD_AUTH`: a wrong value for an object without DA protection.
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

/// The last `Esys_…(0x…)` code tpm2-tools reported.
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

/// A TPM that answers and throttles wrong authorisation values.
pub fn usable(runner: &dyn Runner) -> bool {
    lockout::read(runner).is_ok_and(|p| p.max_tries > 0 && p.recovery_time > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(stderr: &str) -> Output {
        Output {
            code: 1,
            stdout: Vec::new(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn tpm_failures_are_told_apart() {
        assert!(matches!(
            classify(&failure(
                "WARNING:esys:src/tss2-esys/api/Esys_Unseal.c:295:Esys_Unseal_Finish() Received TPM Error\n\
                 ERROR:esys:src/tss2-esys/api/Esys_Unseal.c:98:Esys_Unseal() Esys Finish ErrorCode (0x0000098e)\n\
                 ERROR: Esys_Unseal(0x98E) - tpm:session(1):the authorization HMAC check failed and DA counter incremented"
            )),
            Failure::Wrong
        ));
        assert!(matches!(
            classify(&failure(
                "ERROR: Esys_Unseal(0x921) - tpm:warn(2.0): authorizations for objects subject to DA protection are not allowed at this time because the TPM is in DA lockout mode"
            )),
            Failure::LockedOut
        ));
        assert!(matches!(
            classify(&failure(
                "ERROR: Esys_Load(0x1DF) - tpm:parameter(1):integrity check failed"
            )),
            Failure::Other
        ));
        // The code decides, whatever words come with it: a wrong value named
        // by its handle, and a text that mentions the lockout.
        assert!(matches!(
            classify(&failure(
                "ERROR: Esys_Unseal(0x18E) - tpm:handle(1):the lockout counter was incremented"
            )),
            Failure::Wrong
        ));
        assert!(matches!(
            classify(&failure("ERROR: no lockout here, and no code either")),
            Failure::Other
        ));
    }
}
