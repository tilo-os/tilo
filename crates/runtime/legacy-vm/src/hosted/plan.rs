//! What an account's plan lets it run, and the refusal when it is more.
//!
//! The service reads the plans file on every admission, so operators change
//! limits without a restart. A refusal is written for the agent that asked:
//! it says what the limit is, what is in the way, and what to do, and in its
//! typed form ([`Refusal::typed`], a [`host_account::Refusal`]) the steps
//! that would let it through, with the provider's pages.
//!
//! A host that sells its VMs says so in the file ([`Plans::sells`]). An
//! account the file then does not name has no card on file: its first VM is
//! refused with [`Refusal::NoPaymentMethod`], and nothing asks for a card
//! before that. The hosting provider adds the account once a card is on
//! file, and the next admission lets it in.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::host_account::{
    self, Invoice, NextStep, NextStepKind, PlanOffer, Reason,
};

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
    /// On a host that sells VMs: the [`PlanOffer::id`] the account is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// On a host that sells VMs: when the account's billing period began,
    /// Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_start_ms: Option<u64>,
    /// On a host that sells VMs: the account's invoices, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invoices: Vec<Invoice>,
    /// On a host that sells VMs: the account's credit, in the provider's
    /// words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
}

impl Default for Plan {
    /// The plan of an account the plans file does not name: one small or
    /// default VM at a time, three kept.
    fn default() -> Self {
        Self {
            max_running: 1,
            max_vms: 3,
            sizes: vec![VmSize::Small, VmSize::Default],
            plan: None,
            period_start_ms: None,
            invoices: Vec::new(),
            credit: None,
        }
    }
}

/// What a host that sells its VMs offers, and the provider's pages. In each
/// page's address, `{account}` stands for the account's id.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Sells {
    /// The host's name, as people know it ("Tilo Cloud").
    pub host: String,
    pub plans: Vec<PlanOffer>,
    /// Where the person adds a card.
    pub add_payment_method: String,
    /// Where the person manages billing: cards, plan, invoices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manage_billing: Option<String>,
    /// Where the person raises the account's limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raise_limit: Option<String>,
}

impl Sells {
    /// `page` for `account`.
    #[must_use]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "`{account}` is the placeholder a provider's page addresses carry, not a format"
    )]
    pub fn page(page: &str, account: &str) -> String {
        page.replace("{account}", account)
    }
}

/// The plans file: a default and per-account plans, and what the host
/// sells, if anything.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Plans {
    #[serde(default)]
    pub default: Plan,
    #[serde(default)]
    pub accounts: BTreeMap<String, Plan>,
    /// Present on a host that sells its VMs: then only the accounts named
    /// in `accounts` (those with a card on file) run any, and `default`
    /// is not anyone's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sells: Option<Sells>,
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

    /// The plan of `account`; `None` on a host that sells its VMs when the
    /// account has no card on file.
    #[must_use]
    pub fn for_account(&self, account: &str) -> Option<&Plan> {
        match (self.accounts.get(account), &self.sells) {
            (Some(plan), _) => Some(plan),
            (None, None) => Some(&self.default),
            (None, Some(_)) => None,
        }
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
    SizeNotInPlan {
        size: VmSize,
        allowed: Vec<VmSize>,
    },
    TooManyRunning {
        limit: u32,
        awake: Vec<AwakeVm>,
    },
    TooManyVms {
        limit: u32,
        existing: u32,
    },
    /// The host sells its VMs and the account has no card on file.
    NoPaymentMethod {
        host: String,
    },
}

/// Whether `vm` (of `size`) may wake for `account` under `plans`. `awake`
/// are the account's VMs that are awake now, `existing` how many VMs
/// (disks) the account has, `vm_exists` whether this one is among them.
pub fn admit(
    plans: &Plans,
    account: &str,
    vm: &str,
    size: VmSize,
    awake: &[AwakeVm],
    existing: u32,
    vm_exists: bool,
) -> Result<(), Refusal> {
    let Some(plan) = plans.for_account(account) else {
        return Err(Refusal::NoPaymentMethod {
            host: plans
                .sells
                .as_ref()
                .map(|sells| sells.host.clone())
                .unwrap_or_default(),
        });
    };
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

impl Refusal {
    /// The refusal as the host's contract carries it: its reason, these
    /// words, and the steps that would let it through, with the pages of
    /// the provider `sells` names for `account`.
    #[must_use]
    pub fn typed(&self, sells: Option<&Sells>, account: &str) -> host_account::Refusal {
        let page = |page: Option<&String>| page.map(|page| Sells::page(page, account));
        let raise = NextStep {
            kind: NextStepKind::RaiseLimit,
            url: page(sells.and_then(|sells| sells.raise_limit.as_ref())),
        };
        let names = |sizes: &[VmSize]| sizes.iter().map(ToString::to_string).collect();
        let (reason, next) = match self {
            Self::NoPaymentMethod { .. } => (
                Reason::NoPaymentMethod,
                vec![NextStep {
                    kind: NextStepKind::AddPaymentMethod,
                    url: page(sells.map(|sells| &sells.add_payment_method)),
                }],
            ),
            Self::SizeNotInPlan { size, allowed } => (
                Reason::SizeNotInPlan {
                    size: size.to_string(),
                    allowed: names(allowed),
                },
                vec![
                    NextStep {
                        kind: NextStepKind::ChooseSize,
                        url: None,
                    },
                    raise,
                ],
            ),
            Self::TooManyRunning { limit, awake } => (
                Reason::TooManyRunning {
                    limit: *limit,
                    running: awake.iter().map(|vm| vm.workspace.clone()).collect(),
                },
                vec![
                    NextStep {
                        kind: NextStepKind::WaitForIdle,
                        url: None,
                    },
                    raise,
                ],
            ),
            Self::TooManyVms { limit, existing } => (
                Reason::TooManyVms {
                    limit: *limit,
                    existing: *existing,
                },
                vec![raise],
            ),
        };
        host_account::Refusal {
            reason,
            message: self.to_string(),
            next,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPaymentMethod { host } => {
                let host = if host.is_empty() { "This host" } else { host };
                write!(
                    f,
                    "This needs a Linux machine. {host} runs Linux machines for accounts with \
                     a card on file, and this account has none yet. Add a card to continue."
                )
            }
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
        let plan = Plans {
            default: Plan {
                max_running: 2,
                max_vms: 3,
                sizes: vec![VmSize::Small, VmSize::Default],
                ..Plan::default()
            },
            ..Plans::default()
        };
        let admit = |vm, size, awake: &[AwakeVm], existing, vm_exists| {
            super::admit(&plan, "acct-1", vm, size, awake, existing, vm_exists)
        };
        // phase: within the plan, and a VM already awake is always let in
        {
            assert_eq!(admit("a/default", VmSize::Small, &[], 0, false), Ok(()));
            let both = [awake("a", VmSize::Small), awake("b", VmSize::Default)];
            assert_eq!(admit("a/default", VmSize::Small, &both, 2, true), Ok(()));
        }
        // phase: a third running VM is refused in plain words
        {
            let both = [awake("a", VmSize::Small), awake("b", VmSize::Default)];
            let refusal = admit("c/default", VmSize::Small, &both, 2, false).unwrap_err();
            assert_eq!(
                refusal.to_string(),
                "Your plan runs at most 2 Linux VMs at a time, and 2 are running now: \
                 a (small), b (default). A VM stops on its own shortly after its last command \
                 ends; try again then, stop one of those, or upgrade the plan."
            );
        }
        // phase: a size outside the plan names what the plan has
        {
            let refusal = admit("a/default", VmSize::Xlarge, &[], 0, true).unwrap_err();
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
            let refusal = admit("d/default", VmSize::Small, &[], 3, false).unwrap_err();
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
            assert_eq!(admit("a/default", VmSize::Small, &[], 3, true), Ok(()));
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
        assert_eq!(plans.for_account("acct-pro").unwrap().max_running, 4);
        assert_eq!(plans.for_account("someone").unwrap().sizes, [VmSize::Small]);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(Plans::load(&path).is_err());
    }

    #[test]
    fn a_host_that_sells_vms_refuses_an_account_without_a_card_with_its_page() {
        let mut plans: Plans = serde_json::from_str(
            r#"{"sells":{"host":"Tilo Cloud",
                "plans":[{"id":"pro","name":"Pro","limits":{"max_running":2,"max_vms":5,"sizes":["small","default"]}}],
                "add_payment_method":"https://billing.example/card?account={account}",
                "raise_limit":"https://billing.example/plan?account={account}"}}"#,
        )
        .unwrap();
        // phase: no card on file, so no VM, and the refusal says where to add one
        {
            let refusal =
                admit(&plans, "acct-1", "a/default", VmSize::Small, &[], 0, false).unwrap_err();
            assert_eq!(
                refusal.to_string(),
                "This needs a Linux machine. Tilo Cloud runs Linux machines for accounts with a \
                 card on file, and this account has none yet. Add a card to continue."
            );
            let typed = refusal.typed(plans.sells.as_ref(), "acct-1");
            assert_eq!(typed.reason, Reason::NoPaymentMethod);
            assert!(typed.awaits_the_account());
            assert_eq!(
                typed
                    .step(NextStepKind::AddPaymentMethod)
                    .and_then(|step| step.url.as_deref()),
                Some("https://billing.example/card?account=acct-1")
            );
        }
        // phase: the provider names the account once its card is on file
        {
            plans.accounts.insert(
                "acct-1".into(),
                Plan {
                    max_running: 1,
                    plan: Some("pro".into()),
                    ..Plan::default()
                },
            );
            assert_eq!(
                admit(&plans, "acct-1", "a/default", VmSize::Small, &[], 0, false),
                Ok(())
            );
            let busy = [AwakeVm {
                vm: "a/default".into(),
                workspace: "a".into(),
                size: VmSize::Small,
            }];
            let typed = admit(
                &plans,
                "acct-1",
                "b/default",
                VmSize::Small,
                &busy,
                1,
                false,
            )
            .unwrap_err()
            .typed(plans.sells.as_ref(), "acct-1");
            assert_eq!(
                typed.reason,
                Reason::TooManyRunning {
                    limit: 1,
                    running: vec!["a".into()]
                }
            );
            assert_eq!(
                typed.next.iter().map(|step| step.kind).collect::<Vec<_>>(),
                [NextStepKind::WaitForIdle, NextStepKind::RaiseLimit]
            );
            assert_eq!(
                typed
                    .step(NextStepKind::RaiseLimit)
                    .and_then(|step| step.url.as_deref()),
                Some("https://billing.example/plan?account=acct-1")
            );
        }
    }
}
