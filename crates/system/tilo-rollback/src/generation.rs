//! The image's own generation number: `tilo.generation=<N>` on the UKI command line, and the
//! same number in `/usr/lib/tilo/generation`.
//!
//! The command line must be baked into the UKI at image build time. That is what makes the
//! number trustworthy: it is inside the image signature and measured into PCR 11 by
//! `systemd-stub`, so relabelling generation 4 as 40 breaks both. A line the bootloader
//! assembles from an editable config, or a number read from a file under `/boot`, is
//! attacker-controlled.
//!
//! Numbering must be dense (`+1` per published image), not merely monotonic: retention is
//! arithmetic on the number, and the floor is an NV counter, so moving it `A` -> `B` costs
//! `B - A` increments.
//!
//! `/usr/lib/tilo/generation` is readable before first boot, which is the only way to learn a
//! staged deployment's number and seal its keyslot. Nothing trusts the file.

use anyhow::{Context, Result, bail};

/// Kernel command line key carrying the generation number.
pub const CMDLINE_KEY: &str = "tilo.generation";

/// Parse the generation out of a `/proc/cmdline`-shaped string. `None` when the key is
/// absent, an error when present but unusable: a typo must not silently downgrade to absent.
/// Returns an error if [`CMDLINE_KEY`] appears with no value, appears more than once, or its
/// value does not parse as a decimal `u64`.
pub fn from_cmdline(cmdline: &str) -> Result<Option<u64>> {
    let mut found = None;
    for tok in cmdline.split_ascii_whitespace() {
        let Some(rest) = tok.strip_prefix(CMDLINE_KEY) else {
            continue;
        };
        let Some(value) = rest.strip_prefix('=') else {
            // `tilo.generationfoo=1` is a different key; a bare `tilo.generation` is a mistake.
            if rest.is_empty() {
                bail!("{CMDLINE_KEY} present on the kernel command line with no value");
            }
            continue;
        };
        let n: u64 = value
            .parse()
            .with_context(|| format!("{CMDLINE_KEY}={value} is not a decimal generation number"))?;
        // Later occurrences win, as in the kernel, but a duplicate is reported as suspicious.
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

/// The running system's generation, from `/proc/cmdline`.
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

/// The `policyRef` for a generation: big-endian `u64`. Signed with the approved policy digest,
/// this binds a sealed token to one generation rather than to any prediction the key ever
/// signed.
pub fn policy_ref(generation: u64) -> [u8; 8] {
    generation.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generations_parse_exactly_or_fail_loudly() {
        {
            let c = "root=/dev/mapper/root tilo.generation=42 quiet";
            assert_eq!(from_cmdline(c).unwrap(), Some(42));
        }
        {
            assert_eq!(from_cmdline("root=x quiet").unwrap(), None);
        }
        {
            assert_eq!(from_cmdline("tilo.generation=0").unwrap(), Some(0));
        }
        {
            assert_eq!(from_cmdline("tilo.generationx=9").unwrap(), None);
            assert_eq!(from_cmdline("nottilo.generation=9").unwrap(), None);
        }
        {
            assert!(from_cmdline("tilo.generation=abc").is_err());
            assert!(from_cmdline("tilo.generation=-1").is_err());
            assert!(from_cmdline("tilo.generation").is_err());
        }
        {
            assert!(from_cmdline("tilo.generation=1 tilo.generation=2").is_err());
        }
        {
            assert_eq!(policy_ref(1), [0, 0, 0, 0, 0, 0, 0, 1]);
            assert_eq!(policy_ref(0x0102_0304_0506_0708), [1, 2, 3, 4, 5, 6, 7, 8]);
        }
    }
}
