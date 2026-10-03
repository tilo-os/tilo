//! TPM dictionary-attack lockout: reading it, tightening it, and giving the reset
//! authorisation a real value.
//!
//! A four-digit PIN is only defensible because the TPM throttles guesses in hardware. Two
//! things have to be true, neither true by default on a merely `tpm2_clear`ed machine:
//!
//! 1. **The reset authorisation must not be empty.** `TPM2_DictionaryAttackLockReset` zeroes
//!    the failure counter and is authorised by `lockoutAuth`; left empty, an attacker resets
//!    the counter after every batch of guesses and a four-digit PIN protects nothing — the
//!    single most important line in this module.
//! 2. **The parameters must be sane.** They're vendor-set and often generous (32 tries with a
//!    10-minute decay is common), so they're read at enrollment and tightened where the
//!    hardware allows, with what actually got applied reported rather than assumed.
//!
//! # The consequence that must be documented, not hidden
//!
//! `lockoutAuth`'s value is written to the **encrypted** root filesystem — right against the
//! threat model (an attacker with a powered-off machine can't read it; one who can read it
//! already has the disk open) — but it creates a circular dependency:
//!
//! > too many wrong PINs → TPM in lockout → PIN cannot unseal → root filesystem unavailable
//! > → `lockoutAuth` unreadable → lockout cannot be reset
//!
//! The way out is the recovery key, which doesn't involve the TPM at all — the design working
//! as intended (recovery key is the guarantee, TPM keyslot is the convenience) — so enrollment
//! **must** show the recovery key and never treat it as optional; a user who discards it and
//! forgets their PIN has lost the data.
//!
//! A second sharp edge: a failed `lockoutAuth` attempt disables `lockoutAuth` itself for
//! `lockoutRecovery` seconds, so a 24-hour `lockoutRecovery` means one fat-fingered reset
//! locks the owner out of the reset path for a day. Disk unlock is unaffected (governed by
//! `maxTries`/`recoveryTime`); the trade is deliberate — a long `lockoutRecovery` is what
//! stops brute-forcing `lockoutAuth` itself.

use crate::argv;
use crate::exec::Runner;
use anyhow::{Result, bail};
use std::ffi::OsStr;

/// What the hardware reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Params {
    /// Failures before the TPM refuses DA-protected authorisations.
    pub max_tries: u32,
    /// Seconds for the failure counter to decrement by one.
    pub recovery_time: u32,
    /// Seconds `lockoutAuth` stays unusable after a failed attempt.
    pub lockout_recovery: u32,
    /// Current failure count.
    pub counter: u32,
    /// Is `lockoutAuth` set to a non-empty value?
    pub lockout_auth_set: bool,
    /// Is the TPM in lockout right now?
    pub in_lockout: bool,
}

/// What we ask for: 8 tries and a 2-hour decay, chosen so a human can mistype a 6-digit PIN
/// a few times freely while capping sustained offline guessing at 12 attempts a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub max_tries: u32,
    pub recovery_time: u32,
    pub lockout_recovery: u32,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            max_tries: 8,
            recovery_time: 7200,
            lockout_recovery: 86400,
        }
    }
}

/// Verdict on a `Params` reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub params: Params,
    pub target: Target,
    /// Problems that make a short PIN indefensible.
    pub critical: Vec<String>,
    /// Things that are looser than we asked for but not fatal.
    pub warnings: Vec<String>,
}

impl Assessment {
    pub fn pin_is_defensible(&self) -> bool {
        self.critical.is_empty()
    }
}

/// Judge a lockout configuration. Pure, so the thresholds are testable.
pub fn assess(params: Params, target: Target) -> Assessment {
    let mut critical = Vec::new();
    let mut warnings = Vec::new();

    if !params.lockout_auth_set {
        critical.push(
            "the dictionary-attack reset authorisation is empty: anyone can call \
             TPM2_DictionaryAttackLockReset and clear the failure counter, so the PIN is not \
             throttled at all"
                .into(),
        );
    }
    if params.max_tries == 0 {
        critical.push(
            "maxTries is 0, which the TPM treats as dictionary-attack protection disabled".into(),
        );
    } else if params.max_tries > target.max_tries {
        warnings.push(format!(
            "maxTries is {} where {} was requested — the hardware would not go lower",
            params.max_tries, target.max_tries
        ));
    }
    if params.recovery_time == 0 {
        critical.push(
            "recoveryTime is 0: the failure counter decays instantly, so guesses are effectively \
             unlimited"
                .into(),
        );
    } else if params.recovery_time < target.recovery_time {
        warnings.push(format!(
            "recoveryTime is {}s where {}s was requested",
            params.recovery_time, target.recovery_time
        ));
    }
    if params.lockout_recovery < target.lockout_recovery {
        warnings.push(format!(
            "lockoutRecovery is {}s where {}s was requested",
            params.lockout_recovery, target.lockout_recovery
        ));
    }
    if params.in_lockout {
        warnings.push(format!(
            "the TPM is in lockout right now (failure counter {})",
            params.counter
        ));
    }
    Assessment {
        params,
        target,
        critical,
        warnings,
    }
}

/// Parse `tpm2_getcap properties-variable` output.
pub fn parse_properties(text: &str) -> Params {
    let mut p = Params::default();
    let mut in_permanent = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("TPM2_PT_PERMANENT") {
            in_permanent = true;
            continue;
        }
        if t.starts_with("TPM2_PT_") && !t.starts_with("TPM2_PT_PERMANENT") {
            in_permanent = false;
        }
        let kv = |key: &str| -> Option<u32> {
            t.strip_prefix(key)
                .and_then(|r| r.trim().strip_prefix(':'))
                .map(str::trim)
                .and_then(|v| {
                    v.strip_prefix("0x")
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .or_else(|| v.parse().ok())
                })
        };
        if let Some(v) = kv("TPM2_PT_MAX_AUTH_FAIL") {
            p.max_tries = v;
        } else if let Some(v) = kv("TPM2_PT_LOCKOUT_INTERVAL") {
            p.recovery_time = v;
        } else if let Some(v) = kv("TPM2_PT_LOCKOUT_RECOVERY") {
            p.lockout_recovery = v;
        } else if let Some(v) = kv("TPM2_PT_LOCKOUT_COUNTER") {
            p.counter = v;
        } else if in_permanent {
            if let Some(v) = kv("lockoutAuthSet") {
                p.lockout_auth_set = v != 0;
            } else if let Some(v) = kv("inLockout") {
                p.in_lockout = v != 0;
            }
        }
    }
    p
}

/// Read the live parameters.
///
/// # Errors
///
/// Returns an error if `tpm2_getcap` fails.
pub fn read(runner: &dyn Runner) -> Result<Params> {
    let out = runner.run("tpm2_getcap", &argv!["properties-variable"], None)?;
    if !out.ok() {
        bail!(
            "reading TPM lockout parameters failed: {}",
            out.last_error_line()
        );
    }
    Ok(parse_properties(&out.stdout_utf8()))
}

/// Tighten the parameters. Needs `lockoutAuth` once it has been set. Returns the parameters
/// as they actually are afterwards, not necessarily what was asked for —
/// `TPM2_DictionaryAttackParameters` may be more restrictive, and some firmware silently clamps.
///
/// # Errors
///
/// Returns an error if `tpm2_dictionarylockout` fails, or if the subsequent [`read`] fails.
pub fn tighten(runner: &dyn Runner, target: Target, lockout_auth: Option<&[u8]>) -> Result<Params> {
    let mt = target.max_tries.to_string();
    let rt = target.recovery_time.to_string();
    let lr = target.lockout_recovery.to_string();
    let mut args = argv![
        "--setup-parameters",
        "--max-tries",
        &mt,
        "--recovery-time",
        &rt,
        "--lockout-recovery-time",
        &lr,
    ];
    let auth_spec;
    if let Some(a) = lockout_auth {
        auth_spec = format!("str:{}", String::from_utf8_lossy(a));
        args.push(OsStr::new("--auth"));
        args.push(OsStr::new(&auth_spec));
    }
    let out = runner.run("tpm2_dictionarylockout", &args, None)?;
    if !out.ok() {
        bail!(
            "tightening the TPM lockout parameters failed: {}",
            out.last_error_line()
        );
    }
    read(runner)
}

/// Set `lockoutAuth`.
///
/// SAFETY-CRITICAL: only call this when the value is already durably recorded somewhere the
/// user can reach. Setting it and losing it means the only way out of a TPM lockout is a
/// full `tpm2_clear` from firmware setup, destroying every sealed keyslot — recoverable with
/// the recovery key, unrecoverable without it.
///
/// # Errors
///
/// Returns an error if `new_auth` is longer than 48 bytes, or if `tpm2_changeauth` fails.
pub fn set_reset_auth(runner: &dyn Runner, new_auth: &[u8], old_auth: Option<&[u8]>) -> Result<()> {
    if new_auth.len() > 48 {
        bail!(
            "a {}-byte reset authorisation is longer than TPM2B_AUTH accepts in practice; keep it \
             at or below 48",
            new_auth.len()
        );
    }
    let new_spec = format!("str:{}", String::from_utf8_lossy(new_auth));
    let mut args = argv!["-c", "lockout", &new_spec];
    let old_spec;
    if let Some(o) = old_auth {
        old_spec = format!("str:{}", String::from_utf8_lossy(o));
        args.push(OsStr::new("-p"));
        args.push(OsStr::new(&old_spec));
    }
    let out = runner.run("tpm2_changeauth", &args, None)?;
    if !out.ok() {
        bail!(
            "setting the dictionary-attack reset authorisation failed: {}",
            out.last_error_line()
        );
    }
    Ok(())
}

/// Length of a generated reset authorisation, in characters: 32 hex chars (128 bits of
/// entropy), deliberately not more. `TPM2B_AUTH` is bounded by the TPM's largest digest, and
/// implementations are stricter in practice (a 64-byte value is refused by `swtpm`, 48 is
/// accepted) — 32 stays comfortably inside every TPM's limit, and for a value throttled by
/// `lockoutRecovery` (hours per attempt), 128 bits is already absurd overkill.
pub const RESET_AUTH_CHARS: usize = 32;

/// A random reset authorisation.
///
/// # Errors
///
/// Returns an error if the operating system's random source fails.
pub fn new_reset_auth() -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let mut b = [0u8; RESET_AUTH_CHARS / 2];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("reading OS randomness: {e}"))?;
    Ok(zeroize::Zeroizing::new(hex::encode(b).into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SWTPM: &str = "\
TPM2_PT_PERMANENT:
  ownerAuthSet:              0
  endorsementAuthSet:        0
  lockoutAuthSet:            0
  disableClear:              0
  inLockout:                 0
TPM2_PT_STARTUP_CLEAR:
  phEnable:                  1
TPM2_PT_LOCKOUT_COUNTER: 0x0
TPM2_PT_MAX_AUTH_FAIL: 0x3
TPM2_PT_LOCKOUT_INTERVAL: 0x3E8
TPM2_PT_LOCKOUT_RECOVERY: 0x3E8
";

    #[test]
    fn lockout_parameters_parse_and_classify_against_the_target() {
        // Phase `parses_the_swtpm_defaults`.
        {
            let p = parse_properties(SWTPM);
            assert_eq!(p.max_tries, 3);
            assert_eq!(p.recovery_time, 1000);
            assert_eq!(p.lockout_recovery, 1000);
            assert_eq!(p.counter, 0);
            assert!(!p.lockout_auth_set);
            assert!(!p.in_lockout);
        }
        // Phase `parses_a_set_lockout_auth`.
        {
            let p = parse_properties(&SWTPM.replace(
                "lockoutAuthSet:            0",
                "lockoutAuthSet:            1",
            ));
            assert!(p.lockout_auth_set);
        }
        // Phase `an_empty_reset_auth_is_critical_not_a_warning`.
        {
            let a = assess(parse_properties(SWTPM), Target::default());
            assert!(!a.pin_is_defensible());
            assert!(
                a.critical
                    .iter()
                    .any(|c| c.contains("reset authorisation is empty")),
                "{:?}",
                a.critical
            );
        }
        // Phase `zero_max_tries_is_critical`.
        {
            let p = Params {
                max_tries: 0,
                recovery_time: 7200,
                lockout_recovery: 86400,
                lockout_auth_set: true,
                ..Default::default()
            };
            let a = assess(p, Target::default());
            assert!(!a.pin_is_defensible());
        }
        // Phase `zero_recovery_time_is_critical`.
        {
            let p = Params {
                max_tries: 8,
                recovery_time: 0,
                lockout_recovery: 86400,
                lockout_auth_set: true,
                ..Default::default()
            };
            assert!(!assess(p, Target::default()).pin_is_defensible());
        }
        // Phase `generous_vendor_defaults_warn_but_do_not_block`.
        {
            // Typical Intel PTT shipping values.
            let p = Params {
                max_tries: 32,
                recovery_time: 600,
                lockout_recovery: 600,
                lockout_auth_set: true,
                ..Default::default()
            };
            let a = assess(p, Target::default());
            assert!(a.pin_is_defensible(), "{:?}", a.critical);
            assert_eq!(a.warnings.len(), 3, "{:?}", a.warnings);
        }
        // Phase `a_configuration_at_target_is_clean`.
        {
            let p = Params {
                max_tries: 8,
                recovery_time: 7200,
                lockout_recovery: 86400,
                lockout_auth_set: true,
                ..Default::default()
            };
            let a = assess(p, Target::default());
            assert!(a.pin_is_defensible());
            assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        }
        // Phase `tighter_than_asked_for_is_not_a_warning`.
        {
            let p = Params {
                max_tries: 4,
                recovery_time: 86400,
                lockout_recovery: 172_800,
                lockout_auth_set: true,
                ..Default::default()
            };
            let a = assess(p, Target::default());
            assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        }
        // Phase `reset_auth_is_random_and_within_the_tpm_auth_size_limit`.
        {
            let a = new_reset_auth().unwrap();
            assert_eq!(a.len(), RESET_AUTH_CHARS);
            assert!(
                a.len() <= 48,
                "TPM2B_AUTH is refused above ~48 bytes on real implementations"
            );
            assert!(a.iter().all(u8::is_ascii_hexdigit));
            assert_ne!(*a, *new_reset_auth().unwrap());
        }
    }
}
