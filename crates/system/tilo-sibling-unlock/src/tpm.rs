//! The TPM attestation key and the quotes early boot makes with it.
//!
//! A machine whose boot chain the owner accepts releases its sibling half
//! without a prompt, and that acceptance is only worth anything if the
//! machine can prove which boot chain it is in. It proves it with a TPM
//! quote over PCR 4 and 7, signed by an attestation key whose public point
//! the person's identity certified in the boot key certificate.
//!
//! The key is created once, at enrolment, under the owner hierarchy's
//! deterministic primary, and its two blobs travel with the device half in
//! the token's material. Each boot rebuilds the same primary and loads the
//! key again: a key that cannot be loaded (another TPM, or a cleared one)
//! simply produces no quote, and the owner is asked as usual.
//!
//! tpm2-tools is driven as the programs the initramfs already carries, like
//! `tilo-rollback` does.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use tilo_unlock_protocol::attest::{self, QUOTED_PCRS, Quote};
use zeroize::Zeroizing;

use crate::exec;
use crate::material::Attestation;

/// Where Linux exposes the TPM with the kernel's resource manager.
pub const TPM_DEVICE: &str = "/dev/tpmrm0";

/// PCR bank and indices a quote covers: the loaded images and the Secure
/// Boot policy.
const PCR_LIST: &str = "sha256:4,7";

/// A TPM to ask. Production uses the machine's own; a test names an emulated
/// one by its TCTI.
#[derive(Clone, Debug)]
pub struct Tpm {
    tcti: Option<String>,
}

impl Tpm {
    /// The machine's TPM, as the kernel's resource manager exposes it.
    #[must_use]
    pub fn system() -> Self {
        Self { tcti: None }
    }

    /// A TPM named by a TCTI string, as `tpm2-tools` takes it.
    #[must_use]
    pub fn at_tcti(tcti: impl Into<String>) -> Self {
        Self {
            tcti: Some(tcti.into()),
        }
    }

    /// Whether there is a TPM to ask. Without one, a machine can still enrol
    /// for sibling unlock; it just cannot be released without a prompt.
    #[must_use]
    pub fn available(&self) -> bool {
        self.tcti.is_some() || Path::new(TPM_DEVICE).exists()
    }

    fn env(&self) -> Vec<(&str, &str)> {
        self.tcti
            .as_deref()
            .map(|tcti| vec![("TPM2TOOLS_TCTI", tcti)])
            .unwrap_or_default()
    }

    fn run<I, S>(&self, program: &str, args: I, stdin: Option<&[u8]>) -> Result<Zeroizing<Vec<u8>>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        exec::run_with(program, args, stdin, &self.env())
    }

    fn check<I, S>(&self, program: &str, args: I, stdin: Option<&[u8]>) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run(program, args, stdin).map(drop)
    }

    /// Flush every transient object and session. A bare `swtpm` has no
    /// resource manager and runs out of object slots, so this runs between
    /// steps; there is usually nothing to flush, so failures are ignored.
    fn flush(&self) {
        for flag in ["-t", "-l", "-s"] {
            let _ = self.check("tpm2_flushcontext", [flag], None);
        }
    }

    /// Create the owner-hierarchy primary the attestation key lives under and
    /// return its transient handle. The primary's seed and template make it
    /// the same object on every boot.
    fn parent(&self, ws: &Path) -> Result<String> {
        self.flush();
        let context = ws.join("parent.ctx");
        self.check(
            "tpm2_createprimary",
            [
                OsStr::new("-C"),
                OsStr::new("o"),
                OsStr::new("-g"),
                OsStr::new("sha256"),
                OsStr::new("-G"),
                OsStr::new("ecc"),
                OsStr::new("-c"),
                context.as_os_str(),
            ],
            None,
        )
        .context("the TPM could not create the attestation key's parent")?;
        self.transient_handle()
    }

    fn transient_handle(&self) -> Result<String> {
        let handles = self.run("tpm2_getcap", ["handles-transient"], None)?;
        let text = String::from_utf8_lossy(&handles);
        text.split_whitespace()
            .find(|word| word.starts_with("0x"))
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the TPM reports no loaded object"))
    }

    /// Load `public` and `private` under a fresh parent and return the path of
    /// the loaded key's context. The parent is flushed so the next context
    /// load has a slot on a TPM with no resource manager.
    fn load(&self, ws: &Path, public: &Path, private: &Path) -> Result<PathBuf> {
        let parent = self.parent(ws)?;
        let context = ws.join("ak.ctx");
        self.check(
            "tpm2_load",
            [
                OsStr::new("-C"),
                ws.join("parent.ctx").as_os_str(),
                OsStr::new("-u"),
                public.as_os_str(),
                OsStr::new("-r"),
                private.as_os_str(),
                OsStr::new("-c"),
                context.as_os_str(),
            ],
            None,
        )
        .context("the TPM could not load the attestation key")?;
        self.check("tpm2_flushcontext", [parent.as_str()], None)
            .context("the TPM could not release the attestation key's parent")?;
        Ok(context)
    }
}

/// A private directory on a filesystem early boot has: `/run` is there before
/// the real root is mounted.
pub fn scratch() -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("tilo-sibling-unlock-tpm-")
        .tempdir_in("/run")
        .or_else(|_| tempfile::tempdir())
        .context("no private scratch directory")
}

/// Create the attestation key under this machine's TPM and return its blobs
/// and SEC1 point.
///
/// # Errors
///
/// Any `tpm2_*` step fails, or the key's public area cannot be read back.
pub fn create_attestation_key(tpm: &Tpm) -> Result<(Attestation, Vec<u8>)> {
    let ws = scratch()?;
    let dir = ws.path();
    let public = dir.join("ak.pub");
    let private = dir.join("ak.priv");
    self::create(tpm, dir, &public, &private)?;
    let context = tpm.load(dir, &public, &private)?;
    let area = dir.join("ak.tss");
    tpm.check(
        "tpm2_readpublic",
        [
            OsStr::new("-c"),
            context.as_os_str(),
            OsStr::new("-f"),
            OsStr::new("tss"),
            OsStr::new("-o"),
            area.as_os_str(),
        ],
        None,
    )
    .context("the TPM could not read the attestation key back")?;
    let area = std::fs::read(&area).context("the attestation key's public area")?;
    let point = attest::sec1_from_tpm_public(&area).map_err(|error| anyhow!("{error}"))?;
    let blobs = Attestation {
        public: hex::encode(std::fs::read(&public).context("the attestation key's public half")?),
        private: hex::encode(
            std::fs::read(&private).context("the attestation key's private half")?,
        ),
    };
    tpm.flush();
    Ok((blobs, point))
}

fn create(tpm: &Tpm, ws: &Path, public: &Path, private: &Path) -> Result<()> {
    let parent = tpm.parent(ws)?;
    tpm.check(
        "tpm2_create",
        [
            OsStr::new("-C"),
            ws.join("parent.ctx").as_os_str(),
            OsStr::new("-G"),
            OsStr::new("ecc256:ecdsa-sha256:null"),
            OsStr::new("-g"),
            OsStr::new("sha256"),
            OsStr::new("-u"),
            public.as_os_str(),
            OsStr::new("-r"),
            private.as_os_str(),
            OsStr::new("-a"),
            OsStr::new("fixedtpm|fixedparent|sensitivedataorigin|userwithauth|restricted|sign"),
        ],
        None,
    )
    .context("the TPM could not create the attestation key")?;
    tpm.check("tpm2_flushcontext", [parent.as_str()], None)
        .context("the TPM could not release the attestation key's parent")
}

/// Quote PCR 4 and 7 for one request, with `qualifying` as the attestation's
/// `extraData`: a recorded quote answers no other challenge.
///
/// # Errors
///
/// The key cannot be loaded, or the TPM cannot quote. The caller treats this
/// as "no attestation" and lets the owner decide.
pub fn quote(tpm: &Tpm, key: &Attestation, qualifying: &[u8; 32]) -> Result<Quote> {
    let (public, private) = key.blobs()?;
    let ws = scratch()?;
    let dir = ws.path();
    let public_file = dir.join("ak.pub");
    let private_file = dir.join("ak.priv");
    std::fs::write(&public_file, &public).context("the attestation key's public half")?;
    std::fs::write(&private_file, &private).context("the attestation key's private half")?;
    let context = tpm.load(dir, &public_file, &private_file)?;
    let attestation = dir.join("attest");
    let signature = dir.join("sig");
    let values = dir.join("pcr");
    tpm.check(
        "tpm2_quote",
        [
            OsStr::new("-c"),
            context.as_os_str(),
            OsStr::new("-l"),
            OsStr::new(PCR_LIST),
            OsStr::new("-q"),
            OsStr::new(&hex::encode(qualifying)),
            OsStr::new("-m"),
            attestation.as_os_str(),
            OsStr::new("-s"),
            signature.as_os_str(),
            OsStr::new("-o"),
            values.as_os_str(),
            OsStr::new("-F"),
            OsStr::new("values"),
            OsStr::new("-g"),
            OsStr::new("sha256"),
        ],
        None,
    )
    .context("the TPM could not quote the boot chain")?;
    let quote = Quote {
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
    };
    tpm.flush();
    Ok(quote)
}

/// The PCR 4 + 7 composite this machine measures now, which is what an
/// enrolment offers as the boot chain to accept.
///
/// # Errors
///
/// The TPM cannot read the PCRs.
pub fn current_chain(tpm: &Tpm) -> Result<[u8; 32]> {
    let ws = scratch()?;
    let values = ws.path().join("pcr");
    tpm.check(
        "tpm2_pcrread",
        [
            OsStr::new(PCR_LIST),
            OsStr::new("-o"),
            values.as_os_str(),
            OsStr::new("-F"),
            OsStr::new("values"),
        ],
        None,
    )
    .context("the TPM could not read the boot chain")?;
    let bytes = std::fs::read(&values).context("the measured PCRs")?;
    let pcrs = attest::pcr_values(&bytes, &QUOTED_PCRS).map_err(|error| anyhow!("{error}"))?;
    Ok(attest::pcr_composite(&pcrs))
}

/// Enrolment needs the device to be able to attest, so a request for
/// unattended unlock on a machine with no TPM fails instead of silently
/// asking the owner for every boot.
///
/// # Errors
///
/// `unattended` was asked for and `tpm` is the machine's absent TPM.
pub fn require(tpm: &Tpm, unattended: bool) -> Result<()> {
    if unattended && !tpm.available() {
        bail!("unattended unlock needs a TPM to attest this machine's boot chain");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unattended_enrolment_accepts_a_present_tpm() {
        assert!(require(&Tpm::at_tcti("none"), true).is_ok());
        assert!(require(&Tpm::system(), false).is_ok());
    }
}
