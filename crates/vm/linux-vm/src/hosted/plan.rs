//! What an account's plan lets it run, and the refusal when it is more. The service
//! reads the plans file on every admission, so operators change limits without a
//! restart. A refusal is written for the agent that asked: it says what the limit
//! is and, in its typed form ([`Refusal::typed`]), the steps and provider pages
//! that would let it through.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::server_account::{
    self, Invoice, NextStep, NextStepKind, PaymentMethod, PlanOffer, Reason,
};

use tilo_server_protocol::hosted_vm::VmSize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub max_running: u32,
    /// VMs (persistent disks) the account keeps, running or not.
    pub max_vms: u32,
    pub sizes: Vec<VmSize>,
    /// On a host that sells VMs: the [`PlanOffer::id`] the account is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// On a host that sells VMs: when the account's billing period began, Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_start_ms: Option<u64>,
    /// On a host that sells VMs: the account's invoices, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invoices: Vec<Invoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
    /// On a host that sells VMs: the card on file, as the provider describes it (brand, last four
    /// digits). An account the file names has a card on file whether or not the provider describes
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<PaymentMethod>,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            max_running: 1,
            max_vms: 3,
            sizes: vec![VmSize::Small, VmSize::Default],
            plan: None,
            period_start_ms: None,
            invoices: Vec::new(),
            credit: None,
            card: None,
        }
    }
}

/// What a host that sells its VMs offers, and the provider's pages. In each page's address,
/// `{account}` stands for the account's id.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Sells {
    pub host: String,
    pub plans: Vec<PlanOffer>,
    pub add_payment_method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manage_billing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raise_limit: Option<String>,
    /// Browser page actions an account without a card on file may run per billing period; `None`
    /// for none, so every Browser action waits for a card. An account the file names has a card and
    /// is never stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_free_actions: Option<u32>,
}

impl Sells {
    #[must_use]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "`{account}` is the placeholder a provider's page addresses carry, not a format"
    )]
    pub fn page(page: &str, account: &str) -> String {
        page.replace("{account}", account)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Plans {
    #[serde(default)]
    pub default: Plan,
    #[serde(default)]
    pub accounts: BTreeMap<String, Plan>,
    /// Present on a host that sells its VMs: then only the accounts named in `accounts` (those with
    /// a card on file) run any, and `default` is not anyone's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sells: Option<Sells>,
}

impl Plans {
    /// The plans at `path`; the defaults when there is no file. A file that does not parse is an
    /// error, never silently the defaults.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// The plan of `account`; `None` on a host that sells its VMs when the account has no card on
    /// file.
    #[must_use]
    pub fn for_account(&self, account: &str) -> Option<&Plan> {
        match (self.accounts.get(account), &self.sells) {
            (Some(plan), _) => Some(plan),
            (None, None) => Some(&self.default),
            (None, Some(_)) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AwakeVm {
    pub vm: String,
    pub workspace: String,
    pub size: VmSize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Refusal {
    SizeNotInPlan { size: VmSize, allowed: Vec<VmSize> },
    TooManyRunning { limit: u32, awake: Vec<AwakeVm> },
    TooManyVms { limit: u32, existing: u32 },
    NoPaymentMethod { host: String },
    BrowserActionsUsed { free: u32, host: String },
}

/// Whether Browser may run a page action for `account` under `plans`, given the page actions it
/// already ran this billing period.
pub fn admit_browser(plans: &Plans, account: &str, used: u64) -> Result<(), Refusal> {
    let Some(sells) = &plans.sells else {
        return Ok(());
    };
    if plans.accounts.contains_key(account) {
        return Ok(());
    }
    let free = sells.browser_free_actions.unwrap_or(0);
    if used < u64::from(free) {
        Ok(())
    } else {
        Err(Refusal::BrowserActionsUsed {
            free,
            host: sells.host.clone(),
        })
    }
}

/// Whether `vm` (of `size`) may wake for `account` under `plans`: `awake` are its VMs awake now,
/// `existing` how many disks it has, `vm_exists` whether this one is among them.
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
    /// The refusal as the host's contract carries it: its reason, these words, and the steps that
    /// would let it through, with the provider pages `sells` names for `account`.
    #[must_use]
    pub fn typed(&self, sells: Option<&Sells>, account: &str) -> server_account::Refusal {
        let page = |page: Option<&String>| page.map(|page| Sells::page(page, account));
        let raise = NextStep {
            kind: NextStepKind::RaiseLimit,
            url: page(sells.and_then(|sells| sells.raise_limit.as_ref())),
        };
        let names = |sizes: &[VmSize]| sizes.iter().map(ToString::to_string).collect();
        let (reason, next) = match self {
            Self::NoPaymentMethod { .. } | Self::BrowserActionsUsed { .. } => (
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
        server_account::Refusal {
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
            Self::BrowserActionsUsed { free, host } => {
                let host = if host.is_empty() { "this host" } else { host };
                write!(
                    f,
                    "Browser runs {} a month free on {host}, and this account used them. \
                     Add a card to continue.",
                    plural(*free, "page action", "page actions")
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
