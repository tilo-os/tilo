//! What an account's plan lets it run, and the refusal when it is more.
//!
//! The service reads the plans file on every admission, so operators change
//! limits without a restart. A refusal is written for the agent that asked:
//! it says what the limit is, what is in the way, and what to do.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::size::VmSize;

/// One plan's limits.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// VMs of the account running at once.
    pub max_running: u32,
    /// VMs (persistent disks) the account keeps, running or not.
    pub max_vms: u32,
    /// The sizes the plan offers.
    pub sizes: Vec<VmSize>,
}

impl Default for Plan {
    /// The plan of an account the plans file does not name: one small or
    /// default VM at a time, three kept.
    fn default() -> Self {
        Self {
            max_running: 1,
            max_vms: 3,
            sizes: vec![VmSize::Small, VmSize::Default],
        }
    }
}

/// The plans file: a default and per-account plans.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Plans {
    #[serde(default)]
    pub default: Plan,
    #[serde(default)]
    pub accounts: BTreeMap<String, Plan>,
}

impl Plans {
    /// The plans at `path`; the defaults when there is no file. A file that
    /// does not parse is an error, never silently the defaults.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    #[must_use]
    pub fn for_account(&self, account: &str) -> &Plan {
        self.accounts.get(account).unwrap_or(&self.default)
    }
}

/// A VM of the account that is awake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AwakeVm {
    pub vm: String,
    pub workspace: String,
    pub size: VmSize,
}

/// Why a VM may not start.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Refusal {
    SizeNotInPlan { size: VmSize, allowed: Vec<VmSize> },
    TooManyRunning { limit: u32, awake: Vec<AwakeVm> },
    TooManyVms { limit: u32, existing: u32 },
}

/// Whether `vm` (of `size`) may wake under `plan`. `awake` are the
/// account's VMs that are awake now, `existing` how many VMs (disks) the
/// account has, `vm_exists` whether this one is among them.
pub fn admit(
    plan: &Plan,
    vm: &str,
    size: VmSize,
    awake: &[AwakeVm],
    existing: u32,
    vm_exists: bool,
) -> Result<(), Refusal> {
    if awake.iter().any(|running| running.vm == vm) {
        return Ok(());
    }
    if !plan.sizes.contains(&size) {
        return Err(Refusal::SizeNotInPlan {
            size,
            allowed: plan.sizes.clone(),
        });
    }
    if !vm_exists && existing >= plan.max_vms {
        return Err(Refusal::TooManyVms {
            limit: plan.max_vms,
            existing,
        });
    }
    if u32::try_from(awake.len()).unwrap_or(u32::MAX) >= plan.max_running {
        return Err(Refusal::TooManyRunning {
            limit: plan.max_running,
            awake: awake.to_vec(),
        });
    }
    Ok(())
}

fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 {
        format!("{count} {one}")
    } else {
        format!("{count} {many}")
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeNotInPlan { size, allowed } => {
                let allowed: Vec<String> = allowed.iter().map(|size| size.describe()).collect();
                if allowed.is_empty() {
                    write!(
                        f,
                        "Your plan includes no Linux VMs, so this command cannot run. \
                         Upgrade the plan to run Linux programs."
                    )
                } else {
                    write!(
                        f,
                        "This workspace's Linux VM is {}, which your plan does not include. \
                         Your plan includes: {}. Choose one of those for the workspace, or \
                         upgrade the plan.",
                        size.describe(),
                        allowed.join(", ")
                    )
                }
            }
            Self::TooManyRunning { limit, awake } => {
                let busy: Vec<String> = awake
                    .iter()
                    .map(|vm| format!("{} ({})", vm.workspace, vm.size))
                    .collect();
                write!(
                    f,
                    "Your plan runs at most {} at a time, and {} running now: {}. \
                     A VM stops on its own shortly after its last command ends; try again \
                     then, stop one of those, or upgrade the plan.",
                    plural(*limit, "Linux VM", "Linux VMs"),
                    if awake.len() == 1 {
                        "1 is".to_string()
                    } else {
                        format!("{} are", awake.len())
                    },
                    busy.join(", ")
                )
            }
            Self::TooManyVms { limit, existing } => write!(
                f,
                "Your plan keeps at most {}, and this account has {}. Delete a workspace's \
                 VM you no longer need, or upgrade the plan.",
                plural(*limit, "Linux VM", "Linux VMs"),
                existing
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn awake(vm: &str, size: VmSize) -> AwakeVm {
        AwakeVm {
            vm: format!("{vm}/default"),
            workspace: vm.to_string(),
            size,
        }
    }

    #[test]
    fn plans_bound_running_vms_sizes_and_disks() {
        let plan = Plan {
            max_running: 2,
            max_vms: 3,
            sizes: vec![VmSize::Small, VmSize::Default],
        };
        // phase: within the plan, and a VM already awake is always let in
        {
            assert_eq!(
                admit(&plan, "a/default", VmSize::Small, &[], 0, false),
                Ok(())
            );
            let both = [awake("a", VmSize::Small), awake("b", VmSize::Default)];
            assert_eq!(
                admit(&plan, "a/default", VmSize::Small, &both, 2, true),
                Ok(())
            );
        }
        // phase: a third running VM is refused in plain words
        {
            let both = [awake("a", VmSize::Small), awake("b", VmSize::Default)];
            let refusal = admit(&plan, "c/default", VmSize::Small, &both, 2, false).unwrap_err();
            assert_eq!(
                refusal.to_string(),
                "Your plan runs at most 2 Linux VMs at a time, and 2 are running now: \
                 a (small), b (default). A VM stops on its own shortly after its last command \
                 ends; try again then, stop one of those, or upgrade the plan."
            );
        }
        // phase: a size outside the plan names what the plan has
        {
            let refusal = admit(&plan, "a/default", VmSize::Xlarge, &[], 0, true).unwrap_err();
            assert_eq!(
                refusal.to_string(),
                "This workspace's Linux VM is xlarge (16 vCPU, 32 GB, 250 GB disk), which your \
                 plan does not include. Your plan includes: small (2 vCPU, 4 GB, 50 GB disk), \
                 default (4 vCPU, 8 GB, 50 GB disk). Choose one of those for the workspace, or \
                 upgrade the plan."
            );
        }
        // phase: a new disk beyond the plan's count is refused, an old one is not
        {
            let refusal = admit(&plan, "d/default", VmSize::Small, &[], 3, false).unwrap_err();
            assert!(matches!(
                refusal,
                Refusal::TooManyVms {
                    limit: 3,
                    existing: 3
                }
            ));
            assert!(
                refusal
                    .to_string()
                    .starts_with("Your plan keeps at most 3 Linux VMs")
            );
            assert_eq!(
                admit(&plan, "a/default", VmSize::Small, &[], 3, true),
                Ok(())
            );
        }
    }

    #[test]
    fn the_plans_file_names_accounts_and_a_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plans.json");
        assert_eq!(Plans::load(&path).unwrap(), Plans::default());
        std::fs::write(
            &path,
            r#"{"default":{"max_running":1,"max_vms":1,"sizes":["small"]},
                "accounts":{"acct-pro":{"max_running":4,"max_vms":10,"sizes":["small","default","large","xlarge"]}}}"#,
        )
        .unwrap();
        let plans = Plans::load(&path).unwrap();
        assert_eq!(plans.for_account("acct-pro").max_running, 4);
        assert_eq!(plans.for_account("someone").sizes, [VmSize::Small]);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(Plans::load(&path).is_err());
    }
}
