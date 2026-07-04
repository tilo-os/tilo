//! Launcher-side Landlock helpers.
//!
//! Landlock is layered on top of bwrap + seccomp, the same as the
//! compositor's per-workspace sandbox. The launcher's job is to:
//!
//! 1. Derive an FS plan from the manifest (read-only paths get the
//!    read access set, read-write paths get the full access set).
//! 2. Install the ruleset on the *child* process, *after* bwrap has
//!    set up the mount namespace but *before* the entrypoint takes
//!    over. bwrap itself needs broad FS access to mount/bind/
//!    pivot_root, so it cannot be Landlocked.
//!
//! Today we expose [`apply_plan`] as the in-process restriction point.
//! Real wiring through bwrap-via-shim is the same pattern the
//! compositor uses (see `shell/src/sandbox/landlock_shim.rs`):
//! bwrap exec a small shim, the shim restricts itself, the shim execs
//! the real binary. The launcher does not yet ship a separate shim
//! binary — it installs Landlock on the parent (via `pre_exec`) right
//! before bwrap takes over. Because Landlock is inherited by every
//! descendant, the bwrap-spawned app inherits the policy.
//!
//! ### Open design question — bwrap's mount namespace
//!
//! Landlock policies are path-based and are evaluated against the
//! *kernel inode* underneath, so the path passed at `PathFd` time
//! must exist on the host. Bwrap creates a new mount namespace and
//! pivots, so the *destination* paths inside the namespace might not
//! match the policy. The compositor solves this by execing a shim
//! inside the namespace; we leave a TODO for the same wiring here
//! and document the implication in the `apply_plan` doc-comment.

use std::path::PathBuf;

use landlock::{
    ABI, Access, AccessFs, AccessNet, BitFlags, PathBeneath, PathFd, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetError, RulesetStatus,
};

/// Cap at V4 (network bind/connect control), same rationale as the
/// compositor shim. Best-effort drops anything the running kernel
/// can't honour.
pub const TARGET_ABI: ABI = ABI::V4;

/// FS plan element. Pure type so `plan_from_manifest` is unit-testable.
#[derive(Debug, Clone)]
pub struct FsRule {
    pub path: PathBuf,
    pub access: BitFlags<AccessFs>,
}

/// Outcome enum mirroring the shim's, used for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockOutcome {
    FullyEnforced,
    PartiallyEnforced,
    NotEnforced,
    Errored,
}

impl LandlockOutcome {
    pub fn describe(self) -> &'static str {
        match self {
            LandlockOutcome::FullyEnforced => "landlock: fully enforced",
            LandlockOutcome::PartiallyEnforced => {
                "landlock: partially enforced (kernel ABI dropped some rights)"
            }
            LandlockOutcome::NotEnforced => "landlock: NOT enforced (kernel lacks Landlock)",
            LandlockOutcome::Errored => "landlock: ruleset construction failed",
        }
    }
}

pub(crate) fn outcome_from_status(status: RulesetStatus) -> LandlockOutcome {
    match status {
        RulesetStatus::FullyEnforced => LandlockOutcome::FullyEnforced,
        RulesetStatus::PartiallyEnforced => LandlockOutcome::PartiallyEnforced,
        RulesetStatus::NotEnforced => LandlockOutcome::NotEnforced,
    }
}

/// Produce the FS access plan for a manifest's read/write lists. The
/// caller is expected to have already expanded `~/` and
/// `<scope=current>` placeholders.
pub fn plan_from_paths(ro: &[PathBuf], rw: &[PathBuf]) -> Vec<FsRule> {
    let abi = TARGET_ABI;
    let read = AccessFs::from_read(abi);
    let all = AccessFs::from_all(abi);
    let mut plan = Vec::with_capacity(ro.len() + rw.len());
    for p in ro {
        plan.push(FsRule {
            path: p.clone(),
            access: read,
        });
    }
    for p in rw {
        plan.push(FsRule {
            path: p.clone(),
            access: all,
        });
    }
    plan
}

/// Build a ruleset and restrict the calling process to it.
///
/// `net_restricted` mirrors the manifest's `sandbox.net.mode == "off"`
/// (deny TCP bind/connect when set, no-op on ABI < 4).
///
/// **Caveat re: bwrap.** This irreversibly restricts the *current*
/// process and every descendant. If called from the launcher's parent
/// process before exec, the policy paths must exist on the host root.
/// If called via a shim inside bwrap's mount namespace, the paths must
/// exist *in* the namespace. The launcher currently picks the parent-
/// side install — see `apply_plan_or_log` for the safe wrapper.
fn build_and_restrict(
    plan: &[FsRule],
    net_restricted: bool,
) -> Result<RestrictionStatus, RulesetError> {
    let abi = TARGET_ABI;
    let mut ruleset = Ruleset::default().handle_access(AccessFs::from_all(abi))?;
    if net_restricted {
        ruleset = ruleset.handle_access(AccessNet::from_all(abi))?;
    }
    let mut created = ruleset.create()?;
    for rule in plan {
        match PathFd::new(&rule.path) {
            Ok(fd) => {
                created = created.add_rule(PathBeneath::new(fd, rule.access))?;
            }
            Err(e) => {
                tracing::debug!(
                    "landlock: skipping rule for {} (cannot open: {e})",
                    rule.path.display()
                );
            }
        }
    }
    created.restrict_self()
}

/// Apply a plan, logging the outcome. Never returns an error: Landlock
/// is strictly defence-in-depth.
pub fn apply_plan_or_log(plan: &[FsRule], net_restricted: bool) -> LandlockOutcome {
    match build_and_restrict(plan, net_restricted) {
        Ok(status) => {
            let outcome = outcome_from_status(status.ruleset);
            tracing::info!(
                "{} ({} rules, net_restricted={net_restricted})",
                outcome.describe(),
                plan.len()
            );
            outcome
        }
        Err(e) => {
            tracing::warn!("landlock: failed to build/apply ruleset: {e}");
            LandlockOutcome::Errored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn plan_maps_ro_to_read_only_access() {
        let ro = vec![PathBuf::from("/usr")];
        let rw = vec![];
        let plan = plan_from_paths(&ro, &rw);
        assert_eq!(plan.len(), 1);
        let abi = TARGET_ABI;
        assert_eq!(plan[0].access, AccessFs::from_read(abi));
        assert!(!plan[0].access.contains(AccessFs::WriteFile));
    }

    #[test]
    fn plan_maps_rw_to_full_access_superset() {
        let ro = vec![PathBuf::from("/a")];
        let rw = vec![PathBuf::from("/b")];
        let plan = plan_from_paths(&ro, &rw);
        let abi = TARGET_ABI;
        let ro_access = plan
            .iter()
            .find(|r| r.path == PathBuf::from("/a"))
            .unwrap()
            .access;
        let rw_access = plan
            .iter()
            .find(|r| r.path == PathBuf::from("/b"))
            .unwrap()
            .access;
        assert!(rw_access.contains(ro_access), "rw is superset of ro");
        assert_eq!(rw_access, AccessFs::from_all(abi));
    }
}
