//! The TPM2 NV monotonic counter that holds the rollback floor.
//!
//! # Index choice
//!
//! [`DEFAULT_NV_INDEX`] sits in the range the TCG registry leaves to the
//! platform owner, one block away from `0x01D81000` where `systemd-pcrlock`
//! keeps its own policy NV index, so the two cannot collide. It is a
//! [`crate::Config`] field rather than a hard constant because a machine that
//! already has something at that index needs to be able to move ours.
//!
//! # Why a counter and not ordinary NV storage
//!
//! `TPM2_NV_Increment` on an index with `nt=counter` is the only write the TPM
//! will perform on it: there is no "set to value" and no decrement. And a
//! counter index that is undefined and redefined does **not** start over — the
//! TPM initialises a newly created counter to the largest value any counter on
//! that TPM has ever held, specifically so that undefine/redefine cannot be
//! used to roll one back. Verified against `swtpm` in `tests/mechanism.rs`.
//!
//! # Floor generation versus counter value
//!
//! A freshly created counter does not start at zero — it starts at whatever that
//! TPM's high-water mark is, which on a fresh TPM is 1 and on a well-used one can
//! be anything. So the floor generation is an *offset*:
//!
//! ```text
//! floor_generation = counter_value - base
//! ```
//!
//! `base` is signed, and routinely negative: enrolling generation 500 on a TPM
//! whose counter reads 1 gives `base = 1 - 498 = -497`. Making it unsigned would
//! force the counter to be incremented up to the generation number before the
//! machine could be enrolled at all — hundreds of NV writes on a first install,
//! and impossible for any generation numbering that is not tiny.
//!
//! `base` is fixed when the counter is first materialised and then recorded in
//! two places: every sealed LUKS2 token, and `/var/lib/qos/rollback/state.json`.
//!
//! `base` is not integrity-critical for the security property. The bound that
//! actually gates an unseal is the absolute operand baked into each token's
//! policy digest, and a policy digest cannot be edited — changing the recorded
//! `base` can only make a future seal wrong or a report misleading, never make a
//! stale token unseal.
//!
//! It *is* availability-critical, though: a `base` that has been tampered upward
//! would make [`FloorState::increments_to`] ask for a huge number of increments
//! and soft-brick the machine into recovery-key-only. Hence
//! [`MAX_INCREMENTS_PER_ADVANCE`] and the cross-check in
//! [`crate::confirm::load_base`].

use crate::argv;
use crate::exec::{Runner, require};
use anyhow::{Context, Result, bail};
use std::path::Path;

/// NV index holding the rollback floor.
pub const DEFAULT_NV_INDEX: u32 = 0x01D9_1100;

/// Attributes for the floor counter.
///
/// `authwrite`/`authread` with an empty auth value: any local caller may
/// increment. That is deliberate and worth being explicit about, because it is a
/// real limit rather than an oversight —
///
/// * The floor can only ever move in the *safe* direction. A caller who
///   increments it cannot make an old image bootable; they can only make fewer
///   images bootable.
/// * The damage available to a local attacker is therefore denial of service:
///   push the floor above the running generation and the machine unlocks by
///   recovery key until it is re-enrolled. That is unpleasant, but it is not a
///   data-loss event, and root can already do worse (`cryptsetup luksKillSlot`).
/// * Making it *not* root-reachable requires an offline authority: define the
///   index with `policywrite` and a `PolicyAuthorize` policy, and have the update
///   signer emit a single-use approval whose policy includes
///   `PolicyNV(counter == F-1)`, so each shipped signature authorises exactly one
///   step and cannot be replayed once taken. That is the right end state and it
///   is not built here; the increment path is structured so it can be added
///   behind `AUTH_POLICY_WRITE` without changing the index's Name (see
///   [`define`]).
///
/// Note that `ownerwrite` is deliberately absent, so owner authorisation alone
/// does not increment the floor.
pub const NV_ATTRS: &str = "authread|authwrite|nt=counter";

/// Refuse to perform more than this many increments in one advance without an
/// explicit override. A legitimate advance moves the floor by a handful of
/// generations; a request for thousands means the recorded `base` is wrong or
/// tampered, and applying it would lock the machine out of its own keyslots.
pub const MAX_INCREMENTS_PER_ADVANCE: u64 = 64;

/// Current floor, in both representations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorState {
    /// Raw counter value in the TPM.
    pub counter: u64,
    /// Counter value corresponding to floor generation 0. Signed, and usually
    /// negative — see the module notes.
    pub base: i64,
}

impl FloorState {
    pub fn floor_generation(&self) -> u64 {
        let f = i128::from(self.counter) - i128::from(self.base);
        u64::try_from(f.max(0)).unwrap_or(u64::MAX)
    }

    /// Absolute `operandB` for `TPM2_PolicyNV` when sealing for generation `g`.
    ///
    /// The policy asserts `counter <= base + g`, i.e. `floor <= g`.
    pub fn operand_for(&self, generation: u64) -> Result<u64> {
        operand(self.base, generation)
    }

    /// How many `TPM2_NV_Increment` calls move the floor to `target`.
    pub fn increments_to(&self, target_floor: u64) -> Result<u64> {
        let target_counter = self.operand_for(target_floor)?;
        if target_counter < self.counter {
            bail!(
                "target floor {target_floor} is below the current floor {} — NV counters do not \
                 decrement",
                self.floor_generation()
            );
        }
        Ok(target_counter - self.counter)
    }
}

/// The counter value that corresponds to floor generation `generation`.
///
/// Shared with the LUKS2 token so that the header and the TPM can never drift
/// apart on the arithmetic.
pub fn operand(base: i64, generation: u64) -> Result<u64> {
    let v = i128::from(base) + i128::from(generation);
    if v < 0 {
        bail!(
            "generation {generation} maps to NV counter value {v}, below zero: this device's \
             recorded counter base ({base}) does not belong to this generation numbering"
        );
    }
    u64::try_from(v).context("generation number overflows the NV counter space")
}

fn idx(index: u32) -> String {
    format!("0x{index:08X}")
}

/// Does the index exist?
pub fn exists(runner: &dyn Runner, index: u32) -> Result<bool> {
    let i = idx(index);
    let out = runner.run("tpm2_nvreadpublic", &argv![&i], None)?;
    Ok(out.ok())
}

/// Define the counter. Idempotent: an existing index is left alone.
///
/// SAFETY-CRITICAL: never `tpm2_nvundefine` here. Undefining the index makes
/// every sealed token permanently unopenable (its policy cannot execute at all),
/// which turns the recovery key from a backstop into the only path.
pub fn define(runner: &dyn Runner, index: u32) -> Result<bool> {
    if exists(runner, index)? {
        return Ok(false);
    }
    let i = idx(index);
    require(
        runner,
        "tpm2_nvdefine",
        &argv![&i, "-C", "o", "-a", NV_ATTRS],
        &format!("defining the rollback floor counter at {i}"),
    )?;
    Ok(true)
}

/// Read the counter, or `None` if it exists but has never been incremented.
pub fn read(runner: &dyn Runner, index: u32, scratch: &Path) -> Result<Option<u64>> {
    let i = idx(index);
    let out_path = scratch.join("nvread.bin");
    let out = runner.run(
        "tpm2_nvread",
        &argv![&i, "-C", &i, "-o", out_path.as_os_str()],
        None,
    )?;
    if !out.ok() {
        // An index that exists but was never written reads as an error; that is
        // the only failure we translate to `None`, and only after confirming the
        // index really is there.
        if exists(runner, index)? {
            return Ok(None);
        }
        bail!(
            "reading the rollback floor counter at {i} failed: {}",
            out.last_error_line()
        );
    }
    let bytes = std::fs::read(&out_path).with_context(|| format!("reading {}", out_path.display()))?;
    let _ = std::fs::remove_file(&out_path);
    decode(&bytes)
}

/// Decode the TPM's 8-byte big-endian counter representation.
pub fn decode(bytes: &[u8]) -> Result<Option<u64>> {
    if bytes.len() < 8 {
        bail!("NV counter read returned {} bytes, expected 8", bytes.len());
    }
    let mut v = [0u8; 8];
    v.copy_from_slice(&bytes[..8]);
    Ok(Some(u64::from_be_bytes(v)))
}

/// One increment.
pub fn increment(runner: &dyn Runner, index: u32) -> Result<()> {
    let i = idx(index);
    require(
        runner,
        "tpm2_nvincrement",
        &argv![&i, "-C", &i],
        &format!("incrementing the rollback floor counter at {i}"),
    )?;
    Ok(())
}

/// Materialise a freshly defined counter and pick `base` so that the floor
/// starts at `initial_floor`.
pub fn materialise(
    runner: &dyn Runner,
    index: u32,
    scratch: &Path,
    initial_floor: u64,
) -> Result<FloorState> {
    if read(runner, index, scratch)?.is_none() {
        increment(runner, index)?;
    }
    let counter = read(runner, index, scratch)?
        .context("the rollback floor counter is still unreadable after an increment")?;
    // `base` is deliberately allowed to go negative: a fresh TPM's counter reads
    // 1, and the generation being enrolled is usually much larger than that.
    let base = i64::try_from(i128::from(counter) - i128::from(initial_floor)).with_context(|| {
        format!(
            "counter {counter} and initial floor {initial_floor} are too far apart to record a \
             counter base"
        )
    })?;
    Ok(FloorState { counter, base })
}

/// Raise the floor by repeated increment, re-reading after each step.
///
/// SAFETY-CRITICAL: callers must have already proved that a token at or above
/// `target_floor` unseals. This function does not and cannot check that — see
/// [`crate::confirm::advance_floor`], which is the only intended caller.
pub fn advance(
    runner: &dyn Runner,
    index: u32,
    scratch: &Path,
    state: FloorState,
    target_floor: u64,
    max_increments: u64,
) -> Result<FloorState> {
    let n = state.increments_to(target_floor)?;
    if n == 0 {
        return Ok(state);
    }
    if n > max_increments {
        bail!(
            "advancing the floor to {target_floor} needs {n} increments, more than the {max_increments} \
             allowed in one step; the recorded counter base ({}) is probably wrong — refusing rather \
             than locking the machine out of its own keyslots",
            state.base
        );
    }
    for _ in 0..n {
        increment(runner, index)?;
    }
    let counter = read(runner, index, scratch)?
        .context("the rollback floor counter became unreadable mid-advance")?;
    let after = FloorState { counter, base: state.base };
    if after.floor_generation() < target_floor {
        bail!(
            "floor advance fell short: wanted generation {target_floor}, counter is now {counter} \
             (floor {})",
            after.floor_generation()
        );
    }
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_is_the_counter_minus_the_base() {
        let s = FloorState { counter: 105, base: 100 };
        assert_eq!(s.floor_generation(), 5);
    }

    #[test]
    fn a_negative_base_is_the_normal_case_on_a_fresh_tpm() {
        // Enrolling generation 500 on a TPM whose counter reads 1.
        let s = FloorState { counter: 1, base: 1 - 498 };
        assert_eq!(s.floor_generation(), 498);
        assert_eq!(s.operand_for(500).unwrap(), 3);
        assert_eq!(s.operand_for(498).unwrap(), 1);
    }

    #[test]
    fn a_base_above_the_counter_reads_as_floor_zero_not_a_wrap() {
        let s = FloorState { counter: 3, base: 100 };
        assert_eq!(s.floor_generation(), 0);
    }

    #[test]
    fn operand_is_absolute() {
        let s = FloorState { counter: 105, base: 100 };
        assert_eq!(s.operand_for(7).unwrap(), 107);
        assert_eq!(s.operand_for(0).unwrap(), 100);
    }

    #[test]
    fn a_generation_that_maps_below_zero_is_refused() {
        let s = FloorState { counter: 1, base: -400 };
        let e = s.operand_for(399).unwrap_err().to_string();
        assert!(e.contains("below zero"), "{e}");
    }

    #[test]
    fn increments_to_counts_the_gap() {
        let s = FloorState { counter: 105, base: 100 };
        assert_eq!(s.increments_to(5).unwrap(), 0);
        assert_eq!(s.increments_to(8).unwrap(), 3);
    }

    #[test]
    fn increments_to_counts_the_gap_with_a_negative_base() {
        let s = FloorState { counter: 1, base: -497 };
        assert_eq!(s.floor_generation(), 498);
        assert_eq!(s.increments_to(498).unwrap(), 0);
        assert_eq!(s.increments_to(500).unwrap(), 2);
    }

    #[test]
    fn increments_to_refuses_to_go_backwards() {
        let s = FloorState { counter: 105, base: 100 };
        let e = s.increments_to(4).unwrap_err().to_string();
        assert!(e.contains("do not decrement"), "{e}");
    }

    #[test]
    fn operand_overflow_is_an_error() {
        let s = FloorState { counter: u64::MAX, base: i64::MAX };
        assert!(s.operand_for(u64::MAX).is_err());
    }

    #[test]
    fn decode_is_big_endian() {
        assert_eq!(decode(&[0, 0, 0, 0, 0, 0, 0, 7]).unwrap(), Some(7));
        assert_eq!(decode(&[1, 0, 0, 0, 0, 0, 0, 0]).unwrap(), Some(1 << 56));
        assert!(decode(&[0, 0]).is_err());
    }

    #[test]
    fn index_is_formatted_the_way_tpm2_tools_expects() {
        assert_eq!(idx(DEFAULT_NV_INDEX), "0x01D91100");
    }
}
