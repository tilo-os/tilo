//! Which generations keep a sealed keyslot, and which floor follows from that.
//!
//! Pure arithmetic, deliberately separated from the TPM and cryptsetup drivers: these are
//! the decisions that decide whether a user can still boot, so they most need to be provable
//! without hardware.

use anyhow::{Result, bail};

/// A floor advance that has been computed but not yet applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorPlan {
    /// Floor implied purely by the retention window.
    pub window_floor: u64,
    /// Floor the update itself declared, if any. Lets a security update jump past a
    /// known-vulnerable generation instead of leaving it inside the window.
    pub declared_floor: Option<u64>,
    /// Floor currently in the TPM.
    pub current_floor: u64,
    /// What the floor will become.
    pub target_floor: u64,
}

impl FloorPlan {
    pub fn advances(&self) -> bool {
        self.target_floor > self.current_floor
    }
    pub fn increments(&self) -> u64 {
        self.target_floor.saturating_sub(self.current_floor)
    }
}

/// Decide the floor to move to when generation `confirmed` is confirmed good.
/// `retained_generations` counts the current generation, so 3 keeps `N`, `N-1` and `N-2`
/// bootable and puts the floor at `N-2`.
///
/// SAFETY-CRITICAL: a declared floor above the generation being confirmed is **refused**,
/// not clamped — clamping would quietly repair an authoring mistake that testing should
/// catch, and letting it through would raise the floor above every existing token, leaving a
/// machine that boots and then asks for the recovery key forever.
///
/// # Errors
///
/// Returns an error if `retained_generations` is `0`, if `declared_floor` exceeds
/// `confirmed`, or if `current_floor` already exceeds `confirmed`.
pub fn plan_floor(
    confirmed: u64,
    current_floor: u64,
    retained_generations: u32,
    declared_floor: Option<u64>,
) -> Result<FloorPlan> {
    if retained_generations == 0 {
        bail!("retained_generations must be at least 1 (the running generation itself)");
    }
    if let Some(d) = declared_floor
        && d > confirmed
    {
        bail!(
            "declared floor {d} is above the generation being confirmed ({confirmed}): applying it \
             would leave no generation able to unseal"
        );
    }
    if current_floor > confirmed {
        bail!(
            "the rollback floor ({current_floor}) is already above the running generation \
             ({confirmed}): refusing to touch it — this machine is unlocking by recovery key and \
             needs re-enrollment, not a further advance"
        );
    }
    let window_floor = confirmed.saturating_sub(u64::from(retained_generations.saturating_sub(1)));
    let target_floor = window_floor
        .max(current_floor)
        .max(declared_floor.unwrap_or(0));
    Ok(FloorPlan {
        window_floor,
        declared_floor,
        current_floor,
        target_floor,
    })
}

/// How many generations *newer* than the running one keep their keyslot.
///
/// At least one, always — the sharp edge: staging an update seals the incoming
/// generation's keyslot before reboot, so confirming `N` normally finds a sealed keyslot for
/// `N+1` already in the header. Treating "newer than running" as stale deletes the keyslot
/// the *next* boot depends on, landing that boot on the recovery key.
///
/// Two rather than one leaves room for a second update staged while the first is still
/// unconfirmed. Beyond that, pruned oldest-first: a newer generation not among the newest
/// two was staged, superseded, and never booted, and re-staging would seal a fresh keyslot anyway.
pub const RETAINED_NEWER_GENERATIONS: usize = 2;

/// Generations whose sealed keyslot should be kept, newest first, when generation `current`
/// is enrolled or confirmed. Two groups, kept for different reasons:
///
/// * up to [`RETAINED_NEWER_GENERATIONS`] generations *newer* than `current` — the staged
///   update whose keyslot the next boot needs;
/// * the `retained_generations` newest at or below `current` — the rollback window itself.
pub fn keep_set(current: u64, present: &[u64], retained_generations: u32) -> Vec<u64> {
    let mut all: Vec<u64> = present.to_vec();
    if !all.contains(&current) {
        all.push(current);
    }
    all.sort_unstable_by(|a, b| b.cmp(a));
    all.dedup();

    let mut newer: Vec<u64> = all.iter().copied().filter(|g| *g > current).collect();
    newer.truncate(RETAINED_NEWER_GENERATIONS);
    let mut window: Vec<u64> = all.into_iter().filter(|g| *g <= current).collect();
    window.truncate(retained_generations as usize);

    newer.extend(window);
    newer
}

/// Generations whose sealed keyslot should be removed: anything outside [`keep_set`].
pub fn drop_set(current: u64, present: &[u64], retained_generations: u32) -> Vec<u64> {
    let keep = keep_set(current, present, retained_generations);
    let mut out: Vec<u64> = present
        .iter()
        .copied()
        .filter(|g| !keep.contains(g))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// SAFETY-CRITICAL guard: is it safe to prune `drop` given `survivors`? Only permitted when
/// at least one *verified* generation slot survives it and the recovery key has been
/// verified — both passed in as facts the caller already established by actually unlocking
/// with them, not as intentions.
///
/// # Errors
///
/// Returns an error if `recovery_verified` is `false`, or if `verified_survivors` is empty
/// while `drop` is not (pruning would leave nothing proven to unlock the device).
pub fn prune_is_safe(
    drop: &[u64],
    verified_survivors: &[u64],
    recovery_verified: bool,
) -> Result<()> {
    if drop.is_empty() {
        return Ok(());
    }
    if !recovery_verified {
        bail!(
            "refusing to remove keyslots: the recovery key has not been verified on this device in \
             this run, and it is the unlock path that must never be lost"
        );
    }
    if verified_survivors.is_empty() {
        bail!(
            "refusing to remove keyslots {drop:?}: no other generation keyslot has been verified \
             to unlock this device, so pruning could leave the recovery key as the only path"
        );
    }
    if let Some(bad) = drop.iter().find(|g| verified_survivors.contains(g)) {
        bail!(
            "internal error: generation {bad} is in both the drop set and the verified survivors"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_retention_window_and_floor_only_move_forward() {
        // Phase `window_of_three_keeps_current_and_two_older`.
        {
            let p = plan_floor(10, 0, 3, None).unwrap();
            assert_eq!(p.window_floor, 8);
            assert_eq!(p.target_floor, 8);
            assert!(p.advances());
            assert_eq!(p.increments(), 8);
        }
        // Phase `window_of_one_pins_the_floor_to_the_running_generation`.
        {
            let p = plan_floor(10, 0, 1, None).unwrap();
            assert_eq!(p.target_floor, 10);
        }
        // Phase `early_generations_do_not_underflow`.
        {
            assert_eq!(plan_floor(0, 0, 3, None).unwrap().target_floor, 0);
            assert_eq!(plan_floor(1, 0, 3, None).unwrap().target_floor, 0);
            assert_eq!(plan_floor(2, 0, 3, None).unwrap().target_floor, 0);
            assert_eq!(plan_floor(3, 0, 3, None).unwrap().target_floor, 1);
        }
        // Phase `the_floor_never_goes_backwards`.
        {
            // Confirmed generation 10 with a window of 3 wants floor 8, but the floor is already 9
            // (a security update jumped it). Stay at 9.
            let p = plan_floor(10, 9, 3, None).unwrap();
            assert_eq!(p.window_floor, 8);
            assert_eq!(p.target_floor, 9);
            assert!(!p.advances());
            assert_eq!(p.increments(), 0);
        }
        // Phase `a_declared_floor_can_jump_past_a_vulnerable_generation`.
        {
            // Generation 10 is a security fix; 8 and 9 are vulnerable. Declaring floor 10 closes the
            // window immediately.
            let p = plan_floor(10, 5, 3, Some(10)).unwrap();
            assert_eq!(p.window_floor, 8);
            assert_eq!(p.target_floor, 10);
        }
        // Phase `a_declared_floor_below_the_window_does_not_lower_it`.
        {
            let p = plan_floor(10, 0, 3, Some(2)).unwrap();
            assert_eq!(p.target_floor, 8);
        }
        // Phase `a_declared_floor_above_the_running_generation_is_refused`.
        {
            let e = plan_floor(10, 0, 3, Some(11)).unwrap_err().to_string();
            assert!(e.contains("above the generation being confirmed"), "{e}");
        }
        // Phase `a_floor_already_above_the_running_generation_is_refused`.
        {
            let e = plan_floor(5, 6, 3, None).unwrap_err().to_string();
            assert!(e.contains("already above the running generation"), "{e}");
        }
        // Phase `zero_retention_is_refused`.
        {
            assert!(plan_floor(5, 0, 0, None).is_err());
        }
    }

    #[test]
    fn keep_and_drop_sets_protect_staged_and_verified_keyslots() {
        // Phase `keep_set_is_newest_first_and_bounded`.
        {
            assert_eq!(keep_set(10, &[7, 8, 9, 10], 3), vec![10, 9, 8]);
            assert_eq!(keep_set(10, &[], 3), vec![10]);
            assert_eq!(keep_set(10, &[9], 3), vec![10, 9]);
        }
        // Phase `a_staged_generations_keyslot_is_never_dropped`.
        {
            // Generation 11 has been staged and its keyslot sealed; confirming 10 must not remove it,
            // or the next boot has nothing to unseal with.
            assert_eq!(keep_set(10, &[8, 9, 10, 11], 3), vec![11, 10, 9, 8]);
            assert_eq!(drop_set(10, &[8, 9, 10, 11], 3), Vec::<u64>::new());
        }
        // Phase `two_staged_generations_are_kept_and_a_third_is_not`.
        {
            assert_eq!(keep_set(10, &[10, 11, 12], 3), vec![12, 11, 10]);
            // 11 and 12 are the newest two; 13 arriving means 11 was superseded without ever booting,
            // and re-staging it would seal a fresh keyslot.
            assert_eq!(drop_set(10, &[10, 11, 12, 13], 3), vec![11]);
        }
        // Phase `drop_set_is_the_complement`.
        {
            assert_eq!(drop_set(10, &[6, 7, 8, 9, 10], 3), vec![6, 7]);
            assert_eq!(drop_set(10, &[9, 10], 3), Vec::<u64>::new());
        }
        // Phase `pruning_needs_a_verified_survivor_and_a_verified_recovery_key`.
        {
            assert!(
                prune_is_safe(&[], &[], false).is_ok(),
                "nothing to prune is always safe"
            );
            assert!(
                prune_is_safe(&[7], &[10], false).is_err(),
                "recovery key unverified"
            );
            assert!(
                prune_is_safe(&[7], &[], true).is_err(),
                "no verified survivor"
            );
            assert!(prune_is_safe(&[7], &[10], true).is_ok());
            assert!(
                prune_is_safe(&[7], &[7, 10], true).is_err(),
                "contradictory sets"
            );
        }
    }
}
