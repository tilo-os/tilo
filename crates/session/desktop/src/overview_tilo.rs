//! The overview of the person's Tilo below their workspaces: where their
//! workspaces run (Hosts & compute), the agents they let in, and their
//! account.
//!
//! Hosting is not a dashboard of its own. A host is a place a workspace
//! runs; what a host offers the person's account, and what it used, is its
//! answer to [`tilo_actions::remote::host_account::ACCOUNT`] ([`HostAccount`]); a plan, its card and
//! its invoices are the person's account with a hosting provider, whose own
//! pages manage them. The overview says the plan, what was used and the credit as separate facts;
//! invoices are Settings' Billing page's. The overview shows that answer only once it means
//! something: a host that is not this device, a plan, or usage
//! ([`hosting_relevant`]). Someone with one device that sells nothing sees no
//! hosting at all.
//!
//! [`rows`] turns what the host knows ([`TiloDesk`]) into the rows the
//! overview shows; a press on one is an [`OverviewPick`] the host carries
//! out, as every pick of the overview is.
//!
//! The agents' rows are the agent door's ([`door::DoorRead`]): the read the
//! host serves, followed while the overview is up, so the rows say who was
//! let in, what each may reach and what is working, as that changes.

pub mod door;

use std::time::Duration;

use tilo_actions::remote::agent_door::{self, Reach};
use tilo_actions::remote::host_account::{HostAccount, HostKind};
use ui::icons::Icon;
use ui::widget::Fact;

use crate::i18n::{t, t_args};
use crate::overview_panel::OverviewPick;

/// The person, as their account line names them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountLine {
    pub name: String,
    /// What else says who they are: their handle, or where they signed in.
    pub detail: Option<String>,
}

/// What the overview shows below the workspaces, as the host knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TiloDesk {
    /// The person's workspaces the host knows that this desk does not hold, as `(id, name)`:
    /// going to one is the host's action `id`.
    pub elsewhere: Vec<(String, String)>,
    /// Each host's answer for the person's account
    /// ([`tilo_actions::remote::host_account::ACCOUNT`]).
    pub hosts: Vec<HostAccount>,
    /// The agent door's answer, when one answers ([`agent_door::DOOR`]).
    pub door: Option<agent_door::Door>,
    pub account: Option<AccountLine>,
    /// The session's own actions the Tilo mark offered (lock, sign out, …), as `(id, label)`.
    pub actions: Vec<(String, String)>,
    /// The host has somewhere to send feedback.
    pub feedback: bool,
}

/// One row below the workspaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TiloRow {
    /// A section's name.
    Caption(String),
    Item(TiloItem),
}

/// A row that says something and may be pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiloItem {
    /// Stable among the rows: what assistive technology and scripts name it by.
    pub key: String,
    pub icon: Option<Icon>,
    pub title: String,
    /// What the row says under its title, as separate facts: a plan and what it used, a reach
    /// and the runs working in it, a handle. Each is laid out on its own.
    pub facts: Vec<Fact>,
    /// What a press on the row asks for.
    pub press: Option<OverviewPick>,
    /// A control at its trailing edge: its label and what it asks for.
    pub action: Option<(String, OverviewPick)>,
}

/// Whether hosting means anything to the person yet: a host that is not one
/// of their devices, a host that sells something, or usage recorded.
#[must_use]
pub fn hosting_relevant(hosts: &[HostAccount]) -> bool {
    hosts.iter().any(|host| {
        host.kind != HostKind::Device
            || host.sells()
            || host
                .standing
                .as_ref()
                .is_some_and(|standing| standing.usage.vm_seconds() > 0)
    })
}

/// How long VMs ran: minutes in the shared duration style, and from an hour on whole hours, the
/// unit a plan includes and bills ("100 hr", never "4 days 4 hr").
fn vm_time(seconds: u64) -> String {
    const HOUR: u64 = 3600;
    if seconds < HOUR {
        return ui::format::duration(Duration::from_secs(seconds));
    }
    t_args(
        "overview-tilo-hours",
        &[("hours", ui::format::count(seconds / HOUR).into())],
    )
}

/// What a workspace's row says of where it runs: its host and, when the
/// host meters it, its VM time this period. `None` when no host the person
/// has hosting for runs it.
#[must_use]
pub fn usage_line(hosts: &[HostAccount], workspace: &str) -> Option<String> {
    if !hosting_relevant(hosts) {
        return None;
    }
    hosts.iter().find_map(|host| {
        let usage = host.standing.as_ref()?.usage.of(workspace)?;
        Some(if usage.vm_seconds > 0 {
            t_args(
                "overview-tilo-workspace-usage",
                &[
                    ("host", host.host.clone().into()),
                    ("time", vm_time(usage.vm_seconds).into()),
                ],
            )
        } else {
            host.host.clone()
        })
    })
}

/// A host's row: what it is, its plan and what it used.
fn host_row(index: usize, host: &HostAccount) -> TiloItem {
    let icon = match host.kind {
        HostKind::Device => Icon::Monitor,
        HostKind::SelfHosted => Icon::HardDrive,
        HostKind::Provider => Icon::Cloud,
    };
    let mut facts: Vec<Fact> = Vec::new();
    match (&host.standing, host.plan()) {
        (Some(standing), Some(plan)) => {
            let used = standing.usage.vm_seconds();
            let included = standing
                .limits
                .as_ref()
                .and_then(|limits| limits.included_vm_seconds);
            facts.push(Fact::text(plan.name.clone()));
            facts.push(Fact::text(match included {
                // One unit for both, the hour a plan includes: "37 of 100 hr".
                Some(included) => t_args(
                    "overview-tilo-plan-used-of",
                    &[
                        ("used", ui::format::count(used / 3600).into()),
                        ("included", ui::format::count(included / 3600).into()),
                    ],
                ),
                None => t_args(
                    "overview-tilo-plan-used",
                    &[("used", ui::format::count(used / 3600).into())],
                ),
            }));
        }
        (Some(standing), None) if standing.payment_method.is_none() => {
            facts.push(Fact::text(t("overview-tilo-no-card")));
        }
        (Some(_), None) => facts.push(Fact::text(t("overview-tilo-no-plan"))),
        (None, _) => facts.push(Fact::text(t(match host.kind {
            HostKind::Device => "overview-tilo-host-device",
            HostKind::SelfHosted => "overview-tilo-host-self",
            HostKind::Provider => "overview-tilo-host-provider",
        }))),
    }
    if let Some(credit) = host.standing.as_ref().and_then(|s| s.credit.clone()) {
        facts.push(Fact::text(t_args(
            "overview-tilo-credit",
            &[("amount", credit.into())],
        )));
    }
    TiloItem {
        key: format!("overview-host:{index}"),
        icon: Some(icon),
        title: host.host.clone(),
        facts,
        press: None,
        action: host.manage_billing.clone().map(|url| {
            (
                t("overview-tilo-manage-billing"),
                OverviewPick::OpenPage(url),
            )
        }),
    }
}

fn agent_row(grant: &agent_door::Grant) -> TiloItem {
    let reach = match &grant.reach {
        Reach::Account => t("overview-tilo-agent-everywhere"),
        Reach::Workspace { name, .. } => name.clone(),
    };
    let mut facts = vec![Fact::text(reach)];
    if !grant.runs.is_empty() {
        facts.push(Fact::text(t_args(
            "overview-tilo-agent-running",
            &[("runs", grant.runs.len().into())],
        )));
    }
    TiloItem {
        key: format!("overview-agent:{}", grant.id),
        icon: Some(Icon::Bot),
        title: grant.agent.clone(),
        facts,
        press: None,
        action: Some((
            t("overview-tilo-revoke"),
            OverviewPick::RevokeGrant(grant.id.clone()),
        )),
    }
}

/// The rows below the workspaces: Hosts & compute once it is relevant, the
/// agents let in when any are, and the account.
#[must_use]
pub fn rows(desk: &TiloDesk) -> Vec<TiloRow> {
    let mut rows = Vec::new();
    if !desk.elsewhere.is_empty() {
        rows.push(TiloRow::Caption(t("overview-tilo-more-workspaces")));
        rows.extend(desk.elsewhere.iter().map(|(id, name)| {
            TiloRow::Item(TiloItem {
                key: format!("overview-elsewhere:{id}"),
                icon: None,
                title: name.clone(),
                facts: Vec::new(),
                press: Some(OverviewPick::HostAction(id.clone())),
                action: None,
            })
        }));
    }
    if hosting_relevant(&desk.hosts) {
        rows.push(TiloRow::Caption(t("overview-tilo-hosts")));
        for (index, host) in desk.hosts.iter().enumerate() {
            rows.push(TiloRow::Item(host_row(index, host)));
        }
    }
    if let Some(door) = desk.door.as_ref().filter(|door| !door.grants.is_empty()) {
        rows.push(TiloRow::Caption(t("overview-tilo-agents")));
        rows.extend(
            door.grants
                .iter()
                .map(|grant| TiloRow::Item(agent_row(grant))),
        );
    }
    rows.push(TiloRow::Caption(t("overview-tilo-account")));
    if let Some(account) = &desk.account {
        rows.push(TiloRow::Item(TiloItem {
            key: "overview-account".into(),
            icon: Some(Icon::User),
            title: account.name.clone(),
            facts: account.detail.clone().map(Fact::text).into_iter().collect(),
            press: Some(OverviewPick::AccountSettings),
            action: None,
        }));
    }
    rows.extend(desk.actions.iter().map(|(id, label)| {
        TiloRow::Item(TiloItem {
            key: format!("overview-action:{id}"),
            icon: None,
            title: label.clone(),
            facts: Vec::new(),
            press: Some(OverviewPick::HostAction(id.clone())),
            action: None,
        })
    }));
    rows.push(TiloRow::Item(TiloItem {
        key: "overview-about".into(),
        icon: Some(Icon::Info),
        title: t("overview-tilo-about"),
        facts: Vec::new(),
        press: Some(OverviewPick::AboutTilo),
        action: None,
    }));
    if desk.feedback {
        rows.push(TiloRow::Item(TiloItem {
            key: "overview-feedback".into(),
            icon: None,
            title: t("tilo-menu-feedback"),
            facts: Vec::new(),
            press: Some(OverviewPick::Feedback),
            action: None,
        }));
    }
    rows
}

#[cfg(test)]
mod tests;
