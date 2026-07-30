//! Confirming a generation, and only then advancing the floor.
//!
//! # The sequence, and why it is in this order
//!
//! ```text
//! bootc upgrade            stage generation N
//! reboot                   boot into N (generation N-1 still fully able to unseal)
//! health checks pass       the system is demonstrably usable
//! qos-rollback confirm     enroll N's keyslot, prove it unseals, THEN raise the floor
//! ```
//!
//! Every ordering constraint here exists because of a way it can go wrong:
//!
//! * **Floor raised at install time** → generation `N` turns out not to boot,
//!   and the fallback has already been destroyed. The machine is inert and needs
//!   the recovery key. This is the failure the whole design is arranged around.
//! * **Floor raised before the new keyslot is proven** → if sealing produced a
//!   token that cannot unseal (wrong PCR prediction, TPM in lockout, a bug here),
//!   raising the floor removes the older tokens' ability to unseal too and
//!   nothing is left.
//! * **Old keyslots pruned before the floor is raised** → harmless, and it is
//!   the order used, because the floor is the thing that cannot be undone.
//!   Pruning is reversible by re-enrolling; an NV increment is not reversible by
//!   anything.
//!
//! # Interaction with bootc's staged updates
//!
//! `bootc` stages a new deployment and makes it the default for the next boot,
//! keeping the previous one as a rollback target. It does not know about
//! generations, so the join is: the confirmation runs on the *booted* system,
//! reads the generation from the sealed command line of the deployment that
//! actually booted, and confirms that. A deployment that fails to boot never
//! reaches its own confirmation, which is precisely the property needed.
//!
//! `bootc` also exposes `bootc status --json` with `staged` and `booted`. When a
//! staged deployment exists, confirmation of the booted one still proceeds — the
//! staged one is a *future* generation and has nothing to do with whether the
//! running one is good.

use crate::counter::{self, FloorState};
use crate::exec::Runner;
use crate::keyslots;
use crate::policy::Workspace;
use crate::retention;
use crate::token::RollbackToken;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Persisted, non-secret bookkeeping.
///
/// The authoritative copies of everything here also live in the LUKS2 tokens;
/// this file exists so `status` is cheap and so a machine with no enrolled
/// generation yet still knows its counter base.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub nv_index: u32,
    /// Counter value corresponding to floor generation 0.
    pub nv_base: u64,
    /// Last generation whose keyslot was enrolled and proven.
    pub last_confirmed: Option<u64>,
}

pub fn state_path() -> PathBuf {
    PathBuf::from("/var/lib/qos/rollback/state.json")
}

pub fn load_state(path: &Path) -> Result<Option<State>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save_state(path: &Path, state: &State) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

/// Establish the counter base, cross-checking every source that claims to know.
///
/// The tokens in the LUKS2 header and the state file must agree. A disagreement
/// means one of them has been tampered with or a previous run was interrupted,
/// and since a wrong base turns into "increment the counter a very large number
/// of times", it is refused rather than resolved by preference.
pub fn load_base(state: Option<&State>, tokens: &[(u32, RollbackToken)]) -> Result<Option<u64>> {
    let mut bases: Vec<(String, u64)> = Vec::new();
    if let Some(s) = state {
        bases.push(("state file".into(), s.nv_base));
    }
    for (id, t) in tokens {
        bases.push((format!("token {id} (generation {})", t.generation), t.nv_base));
    }
    let Some((_, first)) = bases.first().cloned() else {
        return Ok(None);
    };
    if let Some((who, bad)) = bases.iter().find(|(_, b)| *b != first) {
        bail!(
            "the recorded NV counter base disagrees between sources ({} says {bad}, {} says \
             {first}); refusing to compute a floor advance from an ambiguous base",
            who,
            bases[0].0
        );
    }
    Ok(Some(first))
}

/// A confirmation that has been checked but not applied.
#[derive(Debug, Clone)]
pub struct Plan {
    pub generation: u64,
    pub floor: retention::FloorPlan,
    pub keep: Vec<u64>,
    pub drop: Vec<u64>,
}

/// Work out what confirming `generation` would do.
pub fn plan(
    generation: u64,
    floor: FloorState,
    present: &[u64],
    retained_generations: u32,
    declared_floor: Option<u64>,
) -> Result<Plan> {
    let fp = retention::plan_floor(
        generation,
        floor.floor_generation(),
        retained_generations,
        declared_floor,
    )?;
    Ok(Plan {
        generation,
        floor: fp,
        keep: retention::keep_set(generation, present, retained_generations),
        drop: retention::drop_set(generation, present, retained_generations),
    })
}

/// SAFETY-CRITICAL: raise the floor, but only after proving the running
/// generation's token unseals *right now*.
///
/// `proof` is the passphrase that was obtained by unsealing the token for the
/// generation being confirmed. Requiring it as an argument means the caller
/// cannot reach this function without having done the unseal — the proof is a
/// value, not a promise.
#[allow(clippy::too_many_arguments)]
pub fn advance_floor(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    plan: &Plan,
    floor: FloorState,
    nv_index: u32,
    proof: &[u8],
    scratch: &Path,
) -> Result<FloorState> {
    if !plan.floor.advances() {
        return Ok(floor);
    }
    // 1. The token we are relying on must actually open the disk.
    if !keyslots::verify_passphrase(runner, ws, device, proof, None)? {
        bail!(
            "refusing to raise the rollback floor: the generation-{} secret does not open {}, so \
             raising the floor would leave nothing able to unseal",
            plan.generation,
            device.display()
        );
    }
    // 2. The target floor must not exceed the generation whose token we proved.
    if plan.floor.target_floor > plan.generation {
        bail!(
            "refusing to raise the rollback floor to {} above the proven generation {}",
            plan.floor.target_floor,
            plan.generation
        );
    }
    counter::advance(
        runner,
        nv_index,
        scratch,
        floor,
        plan.floor.target_floor,
        counter::MAX_INCREMENTS_PER_ADVANCE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Prediction;

    fn tok(gen_num: u64, base: u64) -> (u32, RollbackToken) {
        (
            gen_num as u32,
            RollbackToken::new(
                1, gen_num, 0x01D9_1100, base + gen_num, base, "sha256", &[7, 11], false,
                b"p", b"q", b"n", b"d",
                &[Prediction { approved_policy: b"a".to_vec(), signature: b"s".to_vec(), label: String::new() }],
            ),
        )
    }

    #[test]
    fn base_comes_from_agreeing_sources() {
        let s = State { nv_index: 1, nv_base: 100, last_confirmed: Some(7) };
        let toks = vec![tok(7, 100), tok(6, 100)];
        assert_eq!(load_base(Some(&s), &toks).unwrap(), Some(100));
    }

    #[test]
    fn a_disagreeing_base_is_refused() {
        let s = State { nv_index: 1, nv_base: 100, last_confirmed: Some(7) };
        let toks = vec![tok(7, 999)];
        let e = load_base(Some(&s), &toks).unwrap_err().to_string();
        assert!(e.contains("disagrees between sources"), "{e}");
    }

    #[test]
    fn no_source_means_no_base_yet() {
        assert_eq!(load_base(None, &[]).unwrap(), None);
    }

    #[test]
    fn a_base_from_tokens_alone_is_usable() {
        assert_eq!(load_base(None, &[tok(7, 42)]).unwrap(), Some(42));
    }

    #[test]
    fn planning_confirms_the_window_and_the_prune() {
        let floor = FloorState { counter: 100, base: 100 }; // floor generation 0
        let p = plan(10, floor, &[7, 8, 9, 10], 3, None).unwrap();
        assert_eq!(p.floor.target_floor, 8);
        assert_eq!(p.keep, vec![10, 9, 8]);
        assert_eq!(p.drop, vec![7]);
    }

    #[test]
    fn planning_honours_a_declared_floor() {
        let floor = FloorState { counter: 100, base: 100 };
        let p = plan(10, floor, &[8, 9, 10], 3, Some(10)).unwrap();
        assert_eq!(p.floor.target_floor, 10);
        // The keyslots are still retained even though the floor closed the
        // window: the floor is what enforces, and keeping the slots means a
        // later re-enrollment does not have to rebuild them from nothing.
        assert_eq!(p.keep, vec![10, 9, 8]);
    }

    #[test]
    fn state_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        let s = State { nv_index: 0x01D9_1100, nv_base: 5, last_confirmed: None };
        assert_eq!(load_state(&p).unwrap(), None);
        save_state(&p, &s).unwrap();
        assert_eq!(load_state(&p).unwrap(), Some(s));
    }
}
