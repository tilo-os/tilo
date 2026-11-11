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
//! let in, what each may reach and what is working, as that changes. An
//! agent is drawn as presence draws it everywhere, so it reads as itself
//! wherever it shows: its own face at the row's edge, the live dot of what
//! it is doing, and — because taking a grant back cannot be undone —
//! Revoke, which asks on its row first ([`Confirm`]). A press on the row
//! itself opens the agent's card, as its face does anywhere else.

pub mod door;

use std::time::Duration;

use tilo_actions::remote::agent_door::{self, Reach};
use tilo_actions::remote::host_account::{HostAccount, HostKind};
use ui::icons::Icon;
use ui::tokens::PresenceHue;
use ui::widget::{Fact, Tone};

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
// The item outgrows a name only where pointers are 64 bits wide; the page's are 32.
#[cfg_attr(
    target_pointer_width = "64",
    expect(
        clippy::large_enum_variant,
        reason = "a section's name and a row stand in one list; the rows are built with the \
                  desk's read and read until they change, so boxing an item would put a pointer \
                  in the way of every row for nothing"
    )
)]
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
    /// The mark at its leading edge: a kind of thing, or whose face it is.
    pub mark: Option<TiloMark>,
    pub title: String,
    /// What the row says under its title, as separate facts: a plan and what it used, a reach
    /// and the runs working in it, a handle. Each is laid out on its own.
    pub facts: Vec<Fact>,
    /// What a press on the row asks for.
    pub press: Option<OverviewPick>,
    /// A control at its trailing edge: its label and what it asks for.
    pub action: Option<(String, OverviewPick)>,
    /// The control cannot be undone, so it asks first, at the control: what is lost, in the
    /// person's words. Nothing is carried out until the person answers there.
    pub confirm: Option<Confirm>,
}

/// What a mark at a row's leading edge is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TiloMark {
    /// A kind of thing: a monitor, a hard drive, an account.
    Icon(Icon),
    /// A face, drawn as presence draws it everywhere.
    Face(TiloFace),
}

/// A face a row wears, in the shape and colour presence gives it: an agent's rounded square in
/// its client's hue, so software never reads as a person at any scale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiloFace {
    /// What it is called: the name its initials, or its mark, stand for.
    pub name: String,
    /// Its stable id, which picks the hue when it wears no colour of its own.
    pub identity: String,
    /// The colour it wears, as presence's identity colours carry it.
    pub colour: (u8, u8, u8),
    /// Software, not a person: the rounded square and the agent mark.
    pub is_agent: bool,
}

/// An irreversible control's question, asked on its row: what the person loses, and the answer
/// that goes ahead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirm {
    /// The question, naming who loses what: "Claude Code loses access to all your workspaces and
    /// stops now".
    pub question: String,
    /// The answer that goes ahead ("Revoke access"); the other answer is always Keep, and is
    /// never skipped: what cannot be undone is always asked.
    pub answer: String,
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
        mark: Some(TiloMark::Icon(icon)),
        title: host.host.clone(),
        facts,
        press: None,
        action: host.manage_billing.clone().map(|url| {
            (
                t("overview-tilo-manage-billing"),
                OverviewPick::OpenPage(url),
            )
        }),
        confirm: None,
    }
}

/// The colour every face of an agent wears ([`PresenceHue::of_agent_client`]), or the one its
/// grant's own id gives it when this build does not know its client.
fn agent_hue(grant: &agent_door::Grant) -> PresenceHue {
    PresenceHue::of_agent_client(&grant.agent)
        .unwrap_or_else(|| PresenceHue::of_identity(&grant.id))
}

/// What a grant may reach: the words its row says it with, and the same words inside the
/// sentence naming what revoking takes.
fn agent_reach(grant: &agent_door::Grant) -> (String, String) {
    match &grant.reach {
        // A fact starts a line, a sentence does not.
        Reach::Account => (
            t("overview-tilo-agent-everywhere"),
            t("overview-tilo-agent-everywhere-lower"),
        ),
        Reach::Workspace { name, .. } => (name.clone(), name.clone()),
    }
}

/// An agent let in: its face, what it may reach, what it is doing now, the way to its card, and
/// the way to take its grant back. Revoking ends its access and its runs, so that control asks
/// first.
fn agent_row(grant: &agent_door::Grant) -> TiloItem {
    let hue = agent_hue(grant);
    let (reach, in_sentence) = agent_reach(grant);
    let mut facts = vec![Fact::text(reach)];
    if grant.browser_pages {
        // Its key's own word on its pages: what it does in Browser without asking each time.
        facts.push(Fact::text(t("overview-tilo-agent-browser-pages")));
    }
    if !grant.runs.is_empty() {
        // What the agent is doing is the row's live state: a dot in its own colour, and the
        // fact that stays when the row is too narrow for both it and what the agent may reach.
        facts.push(
            Fact::dot(
                Tone::Presence(hue),
                t_args(
                    "overview-tilo-agent-working",
                    &[("runs", grant.runs.len().into())],
                ),
            )
            .priority(1),
        );
    }
    TiloItem {
        key: format!("overview-agent:{}", grant.id),
        mark: Some(TiloMark::Face(TiloFace {
            name: grant.agent.clone(),
            identity: grant.id.clone(),
            colour: ui::presence::identity_rgb(hue),
            is_agent: true,
        })),
        title: grant.agent.clone(),
        facts,
        press: Some(OverviewPick::OpenAgent(grant.agent.clone())),
        action: Some((
            t("overview-tilo-revoke"),
            OverviewPick::RevokeGrant(grant.id.clone()),
        )),
        confirm: Some(Confirm {
            question: t_args(
                "overview-tilo-revoke-question",
                &[
                    ("agent", grant.agent.clone().into()),
                    ("reach", in_sentence.into()),
                ],
            ),
            answer: t("overview-tilo-revoke-access"),
        }),
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
                mark: None,
                title: name.clone(),
                facts: Vec::new(),
                press: Some(OverviewPick::HostAction(id.clone())),
                action: None,
                confirm: None,
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
            mark: Some(TiloMark::Icon(Icon::User)),
            title: account.name.clone(),
            facts: account.detail.clone().map(Fact::text).into_iter().collect(),
            press: Some(OverviewPick::AccountSettings),
            action: None,
            confirm: None,
        }));
    }
    rows.extend(desk.actions.iter().map(|(id, label)| {
        TiloRow::Item(TiloItem {
            key: format!("overview-action:{id}"),
            mark: None,
            title: label.clone(),
            facts: Vec::new(),
            press: Some(OverviewPick::HostAction(id.clone())),
            action: None,
            confirm: None,
        })
    }));
    rows.push(TiloRow::Item(TiloItem {
        key: "overview-about".into(),
        mark: Some(TiloMark::Icon(Icon::Info)),
        title: t("overview-tilo-about"),
        facts: Vec::new(),
        press: Some(OverviewPick::AboutTilo),
        action: None,
        confirm: None,
    }));
    if desk.feedback {
        rows.push(TiloRow::Item(TiloItem {
            key: "overview-feedback".into(),
            mark: None,
            title: t("tilo-menu-feedback"),
            facts: Vec::new(),
            press: Some(OverviewPick::Feedback),
            action: None,
            confirm: None,
        }));
    }
    rows
}

#[cfg(test)]
mod tests;
