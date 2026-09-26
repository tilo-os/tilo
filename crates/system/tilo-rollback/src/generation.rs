//! The image's own generation number.
//!
//! # The interface this expects from the boot chain
//!
//! The generation number is read from the kernel command line as `tilo.generation=<N>` (a
//! decimal `u64`). It **must** be baked into the UKI's `.cmdline` section at image build
//! time, not appended by the bootloader and not written into a file under `/boot` — that is
//! the entire reason it can be trusted: the UKI command line is inside the image signature
//! and measured into PCR 11 by `systemd-stub`, so an attacker cannot relabel generation 4 as
//! 40 without invalidating both the signature and the measurement.
//!
//! Consequences if that contract is broken:
//!
//! * command line assembled by the bootloader from an editable config → the number is
//!   attacker-controlled and bounded rollback is decorative;
//! * number read from a file on `/boot` or `/etc` → same, and worse, because it looks measured;
//! * number absent → [`from_cmdline`] returns `None` and every caller that needs it fails
//!   loudly rather than defaulting to zero, which would seal a token whose floor bound is
//!   trivially satisfiable.
//!
//! # The numbering has to be dense, not merely monotonic
//!
//! `+1` per published image. A build timestamp is monotonic and still wrong, because the
//! retention window is arithmetic on the number ("keep the last three" means `N`, `N-1`,
//! `N-2`) — a CI build counter is the intended source. Also, the floor is a TPM NV counter,
//! and moving it from `A` to `B` costs `B - A` `TPM2_NV_Increment` calls; sparse numbering
//! would turn a one-step advance into millions of NV writes, which
//! [`crate::counter::MAX_INCREMENTS_PER_ADVANCE`] refuses outright.
//!
//! # Where it has to be readable from
//!
//! Two places, for two different jobs:
//!
//! * `tilo.generation=<N>` on the **sealed UKI command line** — the authoritative copy, used
//!   at unlock and confirmation time. Measured, so it cannot be forged.
//! * `/usr/lib/tilo/generation` in the image's filesystem — a plain file holding the same
//!   number, the only way to learn a *staged* deployment's generation (needed since its
//!   keyslot must be sealed before the reboot that first runs it). Not trusted for anything:
//!   at that moment the machine is already unlocked, and the seal it produces is bound to the
//!   measurements of the image that ships the file.

use anyhow::{Context, Result, bail};

/// Kernel command line key carrying the generation number.
pub const CMDLINE_KEY: &str = "tilo.generation";

/// Parse the generation out of a `/proc/cmdline`-shaped string. Returns `None` when the key
/// is absent; returns an error when present but unusable — an unparseable generation is a
/// boot-chain defect, and treating it as "absent" would let a typo downgrade protection
/// silently.
///
/// # Errors
///
/// Returns an error if [`CMDLINE_KEY`] appears with no value, appears more than once, or its
/// value does not parse as a decimal `u64`.
pub fn from_cmdline(cmdline: &str) -> Result<Option<u64>> {
    let mut found = None;
    for tok in cmdline.split_ascii_whitespace() {
        let Some(rest) = tok.strip_prefix(CMDLINE_KEY) else {
            continue;
        };
        let Some(value) = rest.strip_prefix('=') else {
            // `tilo.generationfoo=1` is a different key; `tilo.generation` with no value is a mistake
            // worth reporting.
            if rest.is_empty() {
                bail!("{CMDLINE_KEY} present on the kernel command line with no value");
            }
            continue;
        };
        let n: u64 = value
            .parse()
            .with_context(|| format!("{CMDLINE_KEY}={value} is not a decimal generation number"))?;
        // Later occurrences win, matching how the kernel treats duplicate parameters, but a
        // duplicate is itself suspicious so it is reported.
        if let Some(prev) = found {
            bail!(
                "{CMDLINE_KEY} appears more than once on the kernel command line ({prev} then {n}) \
                 — the sealed command line must carry exactly one"
            );
        }
        found = Some(n);
    }
    Ok(found)
}

/// Read the running system's generation from `/proc/cmdline`.
///
/// # Errors
///
/// Returns an error if `/proc/cmdline` cannot be read, if it fails [`from_cmdline`]'s checks,
/// or if it has no [`CMDLINE_KEY`] at all.
pub fn running() -> Result<u64> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").context("reading /proc/cmdline")?;
    from_cmdline(&cmdline)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no {CMDLINE_KEY}= on the kernel command line: this image was not built with a \
             generation number, so rollback protection cannot be bound to it"
        )
    })
}

/// The `policyRef` bytes for a generation: big-endian `u64`. Signed by the prediction
/// authority together with the approved policy digest, this is what binds a sealed token to
/// one specific generation rather than "any prediction this key ever signed" — see the
/// crate-level notes on `TPM2_PolicyAuthorize`.
pub fn policy_ref(generation: u64) -> [u8; 8] {
    generation.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generations_parse_exactly_or_fail_loudly() {
        // Phase `reads_the_generation`.
        {
            let c = "root=/dev/mapper/root tilo.generation=42 quiet";
            assert_eq!(from_cmdline(c).unwrap(), Some(42));
        }
        // Phase `absent_is_none_not_zero`.
        {
            assert_eq!(from_cmdline("root=x quiet").unwrap(), None);
        }
        // Phase `zero_is_a_legal_generation`.
        {
            assert_eq!(from_cmdline("tilo.generation=0").unwrap(), Some(0));
        }
        // Phase `similar_keys_are_not_confused`.
        {
            assert_eq!(from_cmdline("tilo.generationx=9").unwrap(), None);
            assert_eq!(from_cmdline("nottilo.generation=9").unwrap(), None);
        }
        // Phase `unparseable_is_an_error_not_a_silent_none`.
        {
            assert!(from_cmdline("tilo.generation=abc").is_err());
            assert!(from_cmdline("tilo.generation=-1").is_err());
            assert!(from_cmdline("tilo.generation").is_err());
        }
        // Phase `duplicates_are_rejected`.
        {
            assert!(from_cmdline("tilo.generation=1 tilo.generation=2").is_err());
        }
        // Phase `policy_ref_is_big_endian`.
        {
            assert_eq!(policy_ref(1), [0, 0, 0, 0, 0, 0, 0, 1]);
            assert_eq!(policy_ref(0x0102_0304_0506_0708), [1, 2, 3, 4, 5, 6, 7, 8]);
        }
    }
}
