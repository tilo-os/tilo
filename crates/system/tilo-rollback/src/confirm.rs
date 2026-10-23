//! Confirming a generation, and only then advancing the floor.
//!
//! # The sequence, and why it is in this order
//!
//! ```text
//! bootc upgrade            stage generation N
//! reboot                   boot into N (generation N-1 still fully able to unseal)
//! health checks pass       the system is demonstrably usable
//! tilo-rollback confirm     enroll N's keyslot, prove it unseals, THEN raise the floor
//! ```
//!
//! Every ordering constraint exists because of a way it can go wrong:
//!
//! * **Floor raised at install time** → if `N` turns out not to boot, the fallback is already
//!   destroyed and the machine is inert, needing the recovery key — the failure the whole
//!   design is arranged around.
//! * **Floor raised before the new keyslot is proven** → a token that can't unseal (wrong PCR
//!   prediction, TPM lockout, a bug) would remove the older tokens' ability too, leaving
//!   nothing.
//! * **Old keyslots pruned before the floor is raised** → harmless, and the order used, since
//!   the floor can't be undone while pruning is reversible by re-enrolling.
//!
//! # Interaction with bootc's staged updates
//!
//! `bootc` stages a deployment and makes it the default next boot, keeping the previous one as
//! a rollback target; it knows nothing about generations, so confirmation runs on the *booted*
//! system, reads the generation from its sealed command line, and confirms that — a deployment
//! that fails to boot never reaches its own confirmation. A staged deployment (`bootc status
//! --json`) is a *future* generation and doesn't affect whether the running one is confirmed.

use crate::counter::{self, FloorState};
use crate::exec::Runner;
use crate::keyslots;
use crate::policy::Workspace;
use crate::retention;
use crate::token::RollbackToken;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Persisted, non-secret bookkeeping. The authoritative copies also live in the LUKS2 tokens;
/// this file exists so `status` is cheap and a machine with no enrolled generation yet still
/// knows its counter base.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub nv_index: u32,
    /// Counter value corresponding to floor generation 0. Signed — see
    /// [`crate::counter`].
    pub nv_base: i64,
    /// Last generation whose keyslot was enrolled and proven.
    pub last_confirmed: Option<u64>,
    /// Keyslots that were an independent unlock path at enrollment (recovery key's, and any
    /// `systemd-cryptenroll` slot), recorded so an unattended confirmation can notice one missing.
    #[serde(default)]
    pub foreign_keyslots: Vec<u32>,
}

pub fn state_path() -> PathBuf {
    PathBuf::from("/var/lib/tilo/rollback/state.json")
}

/// # Errors
///
/// Returns an error if `path` exists but cannot be read or parsed; a missing file is not an
/// error and yields `None`.
pub fn load_state(path: &Path) -> Result<Option<State>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// # Errors
///
/// Returns an error if the parent directory cannot be created, or the file cannot be written.
pub fn save_state(path: &Path, state: &State) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(state)?;
    tilo_files::native::atomic::write_with_mode(path, json.as_bytes(), 0o600)
        .with_context(|| format!("atomically writing {}", path.display()))
}

/// Establish the counter base, cross-checking every source that claims to know. The LUKS2
/// tokens and the state file must agree — disagreement means tampering or an interrupted
/// prior run, and since a wrong base turns into "increment the counter a huge number of
/// times", it's refused rather than resolved by preference.
pub fn load_base(state: Option<&State>, tokens: &[(u32, RollbackToken)]) -> Result<Option<i64>> {
    let mut bases: Vec<(String, i64)> = Vec::new();
    if let Some(s) = state {
        bases.push(("state file".into(), s.nv_base));
    }
    for (id, t) in tokens {
        bases.push((
            format!("token {id} (generation {})", t.generation),
            t.nv_base,
        ));
    }
    let Some((first_source, first)) = bases.first().cloned() else {
        return Ok(None);
    };
    if let Some((who, bad)) = bases.iter().find(|(_, b)| *b != first) {
        bail!(
            "the recorded NV counter base disagrees between sources ({who} says {bad}, \
             {first_source} says {first}); refusing to compute a floor advance from an \
             ambiguous base"
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
///
/// # Errors
///
/// Returns an error if `retained_generations` is `0`, if `declared_floor` exceeds
/// `generation`, or if `floor` is already above `generation` — see
/// [`retention::plan_floor`].
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

/// SAFETY-CRITICAL: raise the floor, but only after proving the running generation's token
/// unseals *right now*. `proof` is a secret already shown to open the device for the
/// generation being confirmed — requiring it as an argument means the caller cannot reach
/// this function without having established that.
///
/// Each step also needs a signed, single-use approval; missing approvals stop the advance
/// early and are reported, never the disk unlocking.
///
/// # Errors
///
/// Returns an error if `proof` does not open `req.device` (refusing to raise the floor past the
/// only proven-working secret), if `plan.floor.target_floor` exceeds `plan.generation`, or if
/// the underlying counter advance fails.
pub fn advance_floor(
    runner: &dyn Runner,
    ws: &Workspace,
    req: &Request<'_>,
    plan: &Plan,
    floor: FloorState,
    approvals: &counter::FloorApprovals<'_>,
    proof: &[u8],
) -> Result<counter::AdvanceOutcome> {
    let device = req.device;
    if !plan.floor.advances() {
        return Ok(counter::AdvanceOutcome {
            floor,
            steps: 0,
            unapproved: Vec::new(),
        });
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
        ws,
        req.cfg.nv_index,
        req.scratch,
        floor,
        plan.floor.target_floor,
        approvals,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{GenerationPolicy, NvConstraint, Prediction};
    use crate::token::SealedObject;

    fn tok(gen_num: u64, base: i64) -> (u32, RollbackToken) {
        (
            u32::try_from(gen_num).unwrap(),
            RollbackToken::new(
                1,
                &GenerationPolicy {
                    generation: gen_num,
                    key_name: b"n",
                    pcr_bank: "sha256",
                    pcrs: &[11],
                    platform_pcrs: &[7],
                    nv: NvConstraint {
                        index: 0x01D9_1100,
                        operand: crate::counter::operand(base, gen_num).unwrap(),
                    },
                    pin: false,
                },
                base,
                &SealedObject {
                    public: b"p",
                    private: b"q",
                    policy_digest: b"d",
                },
                &[Prediction {
                    approved_policy: b"a".to_vec(),
                    signature: b"s".to_vec(),
                    label: String::new(),
                }],
            ),
        )
    }

    #[test]
    fn confirmation_bases_come_only_from_agreeing_sources() {
        {
            let s = State {
                nv_index: 1,
                nv_base: 100,
                last_confirmed: Some(7),
                foreign_keyslots: vec![0],
            };
            let toks = vec![tok(7, 100), tok(6, 100)];
            assert_eq!(load_base(Some(&s), &toks).unwrap(), Some(100));
        }
        {
            let s = State {
                nv_index: 1,
                nv_base: 100,
                last_confirmed: Some(7),
                foreign_keyslots: vec![0],
            };
            let toks = vec![tok(7, 999)];
            let e = load_base(Some(&s), &toks).unwrap_err().to_string();
            assert!(e.contains("disagrees between sources"), "{e}");
        }
        {
            assert_eq!(load_base(None, &[]).unwrap(), None);
        }
        {
            assert_eq!(load_base(None, &[tok(7, 42)]).unwrap(), Some(42));
        }
    }

    #[test]
    fn confirmation_plans_window_prune_floor_and_round_trip_state() {
        {
            let floor = FloorState {
                counter: 100,
                base: 100,
            }; // floor generation 0
            let p = plan(10, floor, &[7, 8, 9, 10], 3, None).unwrap();
            assert_eq!(p.floor.target_floor, 8);
            assert_eq!(p.keep, vec![10, 9, 8]);
            assert_eq!(p.drop, vec![7]);
        }
        {
            let floor = FloorState {
                counter: 100,
                base: 100,
            };
            let p = plan(10, floor, &[8, 9, 10], 3, Some(10)).unwrap();
            assert_eq!(p.floor.target_floor, 10);
            // The keyslots are still retained even though the floor closed the window — the floor
            // enforces, and keeping slots means a later re-enrollment doesn't rebuild from nothing.
            assert_eq!(p.keep, vec![10, 9, 8]);
        }
        {
            use std::os::unix::fs::PermissionsExt;

            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("s.json");
            let s = State {
                nv_index: 0x01D9_1100,
                nv_base: 5,
                last_confirmed: None,
                foreign_keyslots: vec![0, 1],
            };
            assert_eq!(load_state(&p).unwrap(), None);
            save_state(&p, &s).unwrap();
            assert_eq!(load_state(&p).unwrap(), Some(s));
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

// The whole confirmation, in one place

/// Inputs to [`run`].
pub struct Request<'a> {
    pub cfg: &'a crate::Config,
    pub device: &'a Path,
    /// Generation being confirmed — the one that booted.
    pub generation: u64,
    /// Directory holding the signed predictions the image shipped.
    pub prediction_dir: &'a Path,
    /// How the independent unlock path is checked, before anything is touched and again after
    /// everything is done.
    pub recovery: keyslots::RecoveryCheck<'a>,
    /// PIN, when the tokens carry one and a person is supplying it.
    pub pin: Option<&'a [u8]>,
    /// A secret already known to open the device — normally the one `unlock` used at boot, left
    /// in `/run`. Supplying it is what makes unattended confirmation possible at all (the sealed
    /// token may need a PIN nothing on the machine knows after boot), and it's a *better* proof
    /// than a fresh unseal, since the machine demonstrably booted with it.
    pub proof: Option<&'a [u8]>,
    /// Raise the floor to at least this generation.
    pub floor_at_least: Option<u64>,
    pub state_path: &'a Path,
    pub scratch: &'a Path,
    /// Report and change nothing.
    pub dry_run: bool,
}

/// What a confirmation did.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub plan: Plan,
    /// `(keyslot, token id)` when this run enrolled the generation.
    pub enrolled: Option<(u32, u32)>,
    pub dropped: Vec<u64>,
    pub floor: FloorState,
    /// Counter values the floor could not reach for want of a signed approval. Non-empty means
    /// protection is running on keyslot retention alone until the authority issues more — the
    /// disk still unlocks.
    pub unapproved_steps: Vec<u64>,
}

/// Refreshed token list, plus the newly-enrolled `(keyslot, token id)` if
/// [`ensure_generation_enrolled`] performed an enrollment.
type EnrolledTokens = (Vec<(u32, RollbackToken)>, Option<(u32, u32)>);

/// Ensure `req.generation` has an enrolled keyslot, enrolling one if it does not. Mirrors the
/// original inline step exactly: the "no signed prediction" check runs even under
/// `req.dry_run` (a dry run still reports whether confirmation would be possible at all), and
/// only the actual enrollment is skipped when dry.
///
/// Returns the token list (refreshed if enrollment happened) and, if enrollment happened,
/// its `(keyslot, token id)`.
fn ensure_generation_enrolled(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    cfg: &crate::Config,
    req: &Request<'_>,
    floor: FloorState,
    authorising: &[u8],
    tokens: Vec<(u32, RollbackToken)>,
) -> Result<EnrolledTokens> {
    if tokens.iter().any(|(_, t)| t.generation == req.generation) {
        return Ok((tokens, None));
    }
    let predictions = crate::policy::load_signed_predictions(req.prediction_dir, req.generation)?;
    if predictions.is_empty() {
        bail!(
            "no signed PCR prediction for generation {} in {}; cannot enroll it, and \
             confirming without enrolling would raise the floor past a generation that has no \
             keyslot",
            req.generation,
            req.prediction_dir.display()
        );
    }
    if req.dry_run {
        return Ok((tokens, None));
    }
    let enrolled = Some(keyslots::enroll_generation(
        runner,
        ws,
        device,
        cfg,
        &keyslots::Enrollment {
            generation: req.generation,
            floor,
            predictions: &predictions,
            pin: req.pin,
        },
        authorising,
    )?);
    let tokens = keyslots::list_tokens(runner, device)?;
    Ok((tokens, enrolled))
}

/// Acquire the secret that proves `req.generation` can unlock this device right now — either
/// the caller-supplied `req.proof` (checked against that generation's own keyslot) or a fresh
/// unseal of it. This value then authorises every destructive step that follows, so no step
/// proceeds on a belief.
fn acquire_proof(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    cfg: &crate::Config,
    req: &Request<'_>,
    tokens: &[(u32, RollbackToken)],
) -> Result<Vec<u8>> {
    let running = tokens
        .iter()
        .find(|(_, t)| t.generation == req.generation)
        .context("no token for the generation being confirmed")?;
    let running_slot = running.1.keyslot()?;
    if let Some(p) = req.proof {
        if !keyslots::verify_passphrase(runner, ws, device, p, Some(running_slot))? {
            bail!(
                "the supplied proof secret does not open generation {}'s keyslot ({running_slot}) \
                 on {} — refusing to confirm a generation on someone else's secret",
                req.generation,
                device.display()
            );
        }
        return Ok(p.to_vec());
    }
    let s = keyslots::unseal(
        runner,
        ws,
        &running.1,
        Path::new(&cfg.pcr_public_key),
        req.pin,
    )?
    .context(
        "the token for the generation being confirmed does not unseal on this machine, so \
         there is nothing to confirm; refusing to touch the floor",
    )?;
    if !keyslots::verify_passphrase(runner, ws, device, &s, None)? {
        bail!(
            "the unsealed secret for generation {} does not open {}",
            req.generation,
            device.display()
        );
    }
    Ok(s.to_vec())
}

/// Confirm a generation: enroll it if needed, prove it, prune the window, and only then raise
/// the floor. This is the ordering the whole design rests on, so it lives in the library
/// rather than the CLI front end — a test that re-implements the order proves nothing about
/// the order the machine actually uses.
///
/// SAFETY-CRITICAL, in the order the steps appear:
///
/// 1. the recovery key must open the device before anything is touched;
/// 2. the generation being confirmed must have a keyslot, enrolled here if not, and
///    enrollment verifies its own slot;
/// 3. its secret must unseal *and* open the device — that value then authorises everything
///    destructive that follows, so no step proceeds on an assumption;
/// 4. pruning comes before the floor: pruning is reversible by re-enrolling, an NV increment
///    is reversible by nothing;
/// 5. the recovery key must still open the device at the end.
///
/// # Errors
///
/// Returns an error at any of the SAFETY-CRITICAL checks above that fails (recovery path,
/// enrollment, proof, or pruning), or if there is no rollback enrollment on this device yet.
pub fn run(runner: &dyn Runner, ws: &Workspace, req: &Request<'_>) -> Result<Outcome> {
    let device = req.device;
    let cfg = req.cfg;

    keyslots::verify_recovery_path(runner, ws, device, &req.recovery)?;
    let authorising: &[u8] = match (req.proof, &req.recovery) {
        (Some(p), _) => p,
        (None, keyslots::RecoveryCheck::Key(k)) => k,
        (None, _) => bail!(
            "confirmation needs either a recovery key or a proof secret that already opens the \
             device; without one there is nothing to authorise a keyslot change with"
        ),
    };

    let tokens = keyslots::list_tokens(runner, device)?;
    let state = load_state(req.state_path)?;
    let base = load_base(state.as_ref(), &tokens)?
        .context("no rollback enrollment on this device yet: run `tilo-rollback enroll` first")?;
    let counter_value = counter::read(runner, cfg.nv_index, req.scratch)?
        .context("the rollback floor counter has never been incremented")?;
    let mut floor = FloorState {
        counter: counter_value,
        base,
    };

    // The generation must have a keyslot before the floor can pass it.
    let (tokens, enrolled) =
        ensure_generation_enrolled(runner, ws, device, cfg, req, floor, authorising, tokens)?;

    let present: Vec<u64> = tokens.iter().map(|(_, t)| t.generation).collect();
    let plan = plan(
        req.generation,
        floor,
        &present,
        cfg.retained_generations,
        req.floor_at_least,
    )?;

    if req.dry_run {
        return Ok(Outcome {
            plan,
            enrolled: None,
            dropped: Vec::new(),
            floor,
            unapproved_steps: Vec::new(),
        });
    }

    // The proof that the generation being confirmed can actually unlock this machine — either
    // handed to us (the secret this boot unlocked with) or unsealed now — authorises everything
    // destructive below, so no step proceeds on a belief.
    let proof = acquire_proof(runner, ws, device, cfg, req, &tokens)?;

    // The `true` is the independent-unlock-path check at the top of this function: it bails
    // rather than returning, so reaching here means it passed.
    retention::prune_is_safe(&plan.drop, &[req.generation], true)?;
    let mut dropped = Vec::new();
    for g in &plan.drop {
        let Some((token_id, t)) = tokens.iter().find(|(_, t)| t.generation == *g) else {
            continue;
        };
        keyslots::kill_generation_slot(runner, ws, device, *token_id, t.keyslot()?, &proof)?;
        dropped.push(*g);
    }

    let authority_key_name =
        crate::policy::key_name(runner, ws, Path::new(&cfg.floor_authority_key))?;
    let approvals = crate::approval::load_all(Path::new(&cfg.approval_dir))?;
    let advanced = advance_floor(
        runner,
        ws,
        req,
        &plan,
        floor,
        &counter::FloorApprovals {
            authority_public_key: Path::new(&cfg.floor_authority_key),
            authority_key_name: &authority_key_name,
            approvals: &approvals,
            dir: Some(Path::new(&cfg.approval_dir)),
        },
        &proof,
    )?;
    floor = advanced.floor;

    save_state(
        req.state_path,
        &State {
            nv_index: cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(req.generation),
            foreign_keyslots: keyslots::foreign_keyslots(runner, device)?,
        },
    )?;

    // SAFETY-CRITICAL: last word goes to the independent unlock path.
    keyslots::verify_recovery_path(runner, ws, device, &req.recovery)?;
    Ok(Outcome {
        plan,
        enrolled,
        dropped,
        floor,
        unapproved_steps: advanced.unapproved,
    })
}
