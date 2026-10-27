//! Sealing the material to this machine's TPM, where it has one.
//!
//! `systemd-creds` with the TPM2 key and PCR 7: the blob opens only on this
//! TPM, and only while the Secure Boot state it was sealed under holds. A
//! copied disk carries a blob nothing else can open. Without a TPM the
//! material is stored in the clear; Settings tells the owner so.

use crate::exec;
use crate::token::Sealing;
use anyhow::{Context, Result};
use base64::Engine as _;
use std::path::Path;
use zeroize::Zeroizing;

/// The credential name bound into the sealed blob.
const CREDENTIAL: &str = "tilo-sibling-unlock";

/// The TPM device whose presence decides the sealing.
pub const TPM_DEVICE: &str = "/dev/tpmrm0";

/// The sealing this machine supports.
pub fn available() -> Sealing {
    if Path::new(TPM_DEVICE).exists() {
        Sealing::Tpm2
    } else {
        Sealing::Cleartext
    }
}

/// Seals `plaintext` and returns it as the token stores it (base64).
pub fn seal(sealing: Sealing, plaintext: &[u8]) -> Result<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    match sealing {
        Sealing::Cleartext => Ok(engine.encode(plaintext)),
        Sealing::Tpm2 => {
            let blob = exec::run(
                "systemd-creds",
                [
                    "encrypt",
                    "--with-key=tpm2",
                    "--tpm2-device=auto",
                    "--tpm2-pcrs=7",
                    &format!("--name={CREDENTIAL}"),
                    "-",
                    "-",
                ],
                Some(plaintext),
            )
            .context("the TPM could not seal the unlock material")?;
            Ok(engine.encode(blob.as_slice()))
        }
    }
}

/// Opens what [`seal`] produced.
///
/// # Errors
/// When the blob is not base64, or the TPM refuses to unseal it (another
/// machine, or a changed Secure Boot state).
pub fn unseal(sealing: Sealing, stored: &str) -> Result<Zeroizing<Vec<u8>>> {
    let bytes = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(stored)
            .context("the stored unlock material is not base64")?,
    );
    match sealing {
        Sealing::Cleartext => Ok(bytes),
        Sealing::Tpm2 => exec::run(
            "systemd-creds",
            ["decrypt", &format!("--name={CREDENTIAL}"), "-", "-"],
            Some(&bytes),
        )
        .context("the TPM could not unseal the unlock material"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleartext_round_trips() {
        let stored = seal(Sealing::Cleartext, b"{\"a\":1}").unwrap();
        assert_eq!(
            unseal(Sealing::Cleartext, &stored).unwrap().as_slice(),
            b"{\"a\":1}"
        );
        assert!(unseal(Sealing::Cleartext, "not base64!").is_err());
    }
}
