//! `tilo-legacy confine-fs`: Landlock a helper to the paths it serves, then
//! exec it. Used for every `virtiofsd` (see [`crate::virtiofs`]).

use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::Command;

use sandbox_core::landlock::{
    LandlockOutcome, NetRestriction, RulesetSpec, TARGET_ABI, access_plan, outcome_from_status,
    restrict_self,
};

/// Restrict this process to `ro`/`rw` and replace it with `program`.
/// Returns only on failure: an unconfined helper is never started.
pub fn exec_confined(ro: &[PathBuf], rw: &[PathBuf], program: &[String]) -> String {
    let spec = RulesetSpec::application(access_plan(ro, rw, TARGET_ABI), NetRestriction::DenyAll);
    let status = match restrict_self(&spec, |_, _| {}) {
        Ok(status) => status,
        Err(error) => return format!("could not confine the helper: {error}"),
    };
    let outcome = outcome_from_status(&status.ruleset);
    if outcome != LandlockOutcome::FullyEnforced {
        return format!(
            "refusing to start an unconfined helper ({})",
            outcome.describe()
        );
    }
    let Some((name, args)) = program.split_first() else {
        return "no program to run".to_string();
    };
    let error = Command::new(name).args(args).exec();
    format!("could not run {name}: {error}")
}
