//! A host's offers to a person's account
//! ([`tilo_actions::remote::host_account`]), as a host that serves
//! workspaces knows them.
//!
//! Such a host runs a workspace's Linux programs in VMs of its own
//! ([`legacy_vm::hosted`]), admitted by its plans file and metered in its
//! usage ledger. Its account answer is read from those two and nothing else:
//!
//! - the plans file says what the host sells, if anything
//!   ([`Plans::sells`]), and the account's plan, limits, billing period and
//!   invoices, which the hosting provider writes there;
//! - the hosted-VM service answers the account's usage records and which of
//!   its VMs run.
//!
//! A host whose plans file sells nothing answers no plans and no standing.
//! [`HostedAccount::watch`] calls back whenever the plans file changes (a
//! card added, a plan changed), so a view following the answer, or a
//! command waiting for it, is told without asking again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Datelike as _, Months, TimeZone as _, Utc};
use legacy_vm::hosted::control::{self, Reply, Request};
use legacy_vm::hosted::meter::{UsageKind, UsageRecord};
use legacy_vm::hosted::plan::{Plans, Sells};
use tilo_actions::remote::host_account::{
    Entitlement, HostAccount, HostKind, Limits, Standing, Usage, WorkspaceUsage,
};

/// The hosted-VM service's socket. Set on a host that serves workspaces.
pub const SOCKET_ENV: &str = "TILO_HOSTED_VM_SOCKET";
/// The account the session's VM time is billed to.
pub const ACCOUNT_ENV: &str = "TILO_HOSTED_ACCOUNT";
/// The plans file the hosted-VM service admits by.
pub const PLANS_ENV: &str = "TILO_HOSTED_PLANS";
/// The host's name, as people know it, when its plans file sells nothing
/// (a host that sells names itself there).
pub const NAME_ENV: &str = "TILO_HOSTED_NAME";

/// How long the service may take to answer a usage question.
const PATIENCE: Duration = Duration::from_secs(5);

/// One account on a host that serves workspaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedAccount {
    pub account: String,
    pub plans: PathBuf,
    /// The hosted-VM service, which answers usage; `None` reads none.
    pub socket: Option<PathBuf>,
    /// The host's name when it sells nothing.
    pub name: String,
}

impl HostedAccount {
    /// The host's account of this session, when the host serves workspaces
    /// ([`ACCOUNT_ENV`] and [`PLANS_ENV`]).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Some(Self {
            account: std::env::var(ACCOUNT_ENV).ok()?,
            plans: PathBuf::from(std::env::var_os(PLANS_ENV)?),
            socket: std::env::var_os(SOCKET_ENV).map(PathBuf::from),
            name: std::env::var(NAME_ENV).unwrap_or_else(|_| "This host".to_owned()),
        })
    }

    /// The account as the host knows it at `now_ms`. A plans file that does
    /// not read answers as a host that sells nothing; the service not
    /// answering, as no usage.
    #[must_use]
    pub fn read(&self, now_ms: u64) -> HostAccount {
        let plans = Plans::load(&self.plans).unwrap_or_default();
        let (from_ms, to_ms) = period(&plans, &self.account, now_ms);
        let usage = self.socket.as_deref().map_or_else(
            || Usage {
                period_start_ms: from_ms,
                period_end_ms: to_ms,
                workspaces: Vec::new(),
            },
            |socket| self.usage(socket, from_ms, to_ms),
        );
        account_of(&plans, &self.account, &self.name, usage)
    }

    fn usage(&self, socket: &Path, from_ms: u64, to_ms: u64) -> Usage {
        let records = match control::ask(
            socket,
            &Request::Usage {
                account: self.account.clone(),
                from_ms,
                to_ms,
            },
            PATIENCE,
        ) {
            Ok(Some(Reply::Usage { records, .. })) => records,
            _ => Vec::new(),
        };
        let running: Vec<String> = match control::ask(socket, &Request::Status, PATIENCE) {
            Ok(Some(Reply::Status(vms))) => vms
                .into_iter()
                .filter(|vm| vm.account == self.account)
                .filter(|vm| matches!(vm.phase.as_str(), "Running" | "Starting"))
                .filter_map(|vm| vm.vm.split('/').next().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        usage_of(&records, &running, from_ms, to_ms)
    }

    /// Call `on_change` whenever the plans file is written, created or
    /// removed. The watch lasts as long as what it returns.
    ///
    /// # Errors
    ///
    /// The plans file's directory cannot be watched.
    pub fn watch(
        &self,
        on_change: impl Fn() + Send + 'static,
    ) -> std::io::Result<tilo_files::native::ChangeWatch> {
        watch_plans(&self.plans, on_change)
    }
}

/// Call `on_change` whenever the plans file at `plans` is written, created
/// or removed.
///
/// # Errors
///
/// Its directory cannot be watched.
pub fn watch_plans(
    plans: &Path,
    on_change: impl Fn() + Send + 'static,
) -> std::io::Result<tilo_files::native::ChangeWatch> {
    let dir = plans
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file = plans.to_path_buf();
    tilo_files::native::watch_changes(dir, move |path| path == file, on_change)
}

/// The billing period of `account` around `now_ms`: from the provider's
/// period start, or the calendar month (UTC), for a month.
fn period(plans: &Plans, account: &str, now_ms: u64) -> (u64, u64) {
    let millis = |at: DateTime<Utc>| u64::try_from(at.timestamp_millis()).unwrap_or(0);
    let at = |ms: u64| {
        Utc.timestamp_millis_opt(i64::try_from(ms).unwrap_or(i64::MAX))
            .single()
            .unwrap_or_default()
    };
    let start = plans
        .accounts
        .get(account)
        .and_then(|plan| plan.period_start_ms)
        .map(at)
        .or_else(|| {
            let now = at(now_ms);
            Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
                .single()
        })
        .unwrap_or_default();
    let end = start.checked_add_months(Months::new(1)).unwrap_or(start);
    (millis(start), millis(end))
}

/// The usage `records` make, per workspace, most used first; `running` are
/// the workspaces whose VMs run now.
#[must_use]
pub fn usage_of(records: &[UsageRecord], running: &[String], from_ms: u64, to_ms: u64) -> Usage {
    let mut workspaces: Vec<WorkspaceUsage> = Vec::new();
    for record in records
        .iter()
        .filter(|record| (from_ms..to_ms).contains(&record.end_ms))
    {
        let index = workspaces
            .iter()
            .position(|usage| usage.workspace == record.identity.workspace)
            .unwrap_or_else(|| {
                workspaces.push(WorkspaceUsage {
                    workspace: record.identity.workspace.clone(),
                    ..WorkspaceUsage::default()
                });
                workspaces.len().saturating_sub(1)
            });
        if let Some(usage) = workspaces.get_mut(index) {
            match record.kind {
                UsageKind::Running => {
                    usage.vm_seconds = usage.vm_seconds.saturating_add(record.seconds);
                }
                UsageKind::Snapshot => {
                    usage.snapshot_seconds = usage.snapshot_seconds.saturating_add(record.seconds);
                }
            }
        }
    }
    for workspace in running {
        match workspaces
            .iter_mut()
            .find(|usage| &usage.workspace == workspace)
        {
            Some(usage) => usage.running = true,
            None => workspaces.push(WorkspaceUsage {
                workspace: workspace.clone(),
                running: true,
                ..WorkspaceUsage::default()
            }),
        }
    }
    workspaces.sort_by_key(|usage| std::cmp::Reverse(usage.vm_seconds));
    Usage {
        period_start_ms: from_ms,
        period_end_ms: to_ms,
        workspaces,
    }
}

/// `account` as a host with `plans` answers it, with `usage`. `name` names
/// a host that sells nothing.
#[must_use]
pub fn account_of(plans: &Plans, account: &str, name: &str, usage: Usage) -> HostAccount {
    let Some(sells) = &plans.sells else {
        return HostAccount::sells_nothing(name, HostKind::SelfHosted);
    };
    let page = |page: &str| Sells::page(page, account);
    let standing = match plans.accounts.get(account) {
        Some(plan) => Standing {
            account: account.to_owned(),
            plan: plan.plan.clone(),
            payment_method: true,
            entitlements: vec![Entitlement::LinuxVms],
            limits: Some(Limits {
                max_running: plan.max_running,
                max_vms: plan.max_vms,
                sizes: plan.sizes.iter().map(ToString::to_string).collect(),
                included_vm_seconds: plan.plan.as_deref().and_then(|id| {
                    sells
                        .plans
                        .iter()
                        .find(|offer| offer.id == id)
                        .and_then(|offer| offer.limits.included_vm_seconds)
                }),
                ..Limits::default()
            }),
            usage,
            credit: plan.credit.clone(),
            invoices: plan.invoices.clone(),
        },
        None => Standing {
            account: account.to_owned(),
            usage,
            ..Standing::default()
        },
    };
    HostAccount {
        host: sells.host.clone(),
        kind: HostKind::Provider,
        plans: sells.plans.clone(),
        standing: Some(standing),
        manage_billing: sells.manage_billing.as_deref().map(page),
    }
}

#[cfg(test)]
mod tests;
