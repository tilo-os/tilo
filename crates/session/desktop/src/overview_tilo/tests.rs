use tilo_actions::remote::agent_door::{Door, Grant, Run};
use tilo_actions::remote::host_account::{
    Entitlement, Invoice, InvoiceStatus, Limits, PaymentMethod, PlanOffer, Standing, Usage,
    WorkspaceUsage,
};

use super::*;

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

fn items(rows: &[TiloRow]) -> Vec<&TiloItem> {
    rows.iter()
        .filter_map(|row| match row {
            TiloRow::Item(item) => Some(item),
            TiloRow::Caption(_) => None,
        })
        .collect()
}

fn facts(item: &TiloItem) -> Vec<&str> {
    item.facts.iter().map(|fact| fact.text.as_str()).collect()
}

fn captions(rows: &[TiloRow]) -> Vec<&str> {
    rows.iter()
        .filter_map(|row| match row {
            TiloRow::Caption(caption) => Some(caption.as_str()),
            TiloRow::Item(_) => None,
        })
        .collect()
}

/// Tilo Cloud selling Pro, with the account on it and a workspace's VMs
/// having run an hour.
fn tilo_cloud() -> HostAccount {
    HostAccount {
        host: "Tilo Cloud".into(),
        kind: HostKind::Provider,
        plans: vec![PlanOffer {
            id: "pro".into(),
            name: "Pro".into(),
            price: Some("$20 a month".into()),
            limits: Limits {
                max_running: 2,
                max_vms: 5,
                sizes: vec!["small".into()],
                included_vm_seconds: Some(100 * 3600),
                ..Limits::default()
            },
        }],
        standing: Some(Standing {
            account: "acct-1".into(),
            plan: Some("pro".into()),
            payment_method: Some(PaymentMethod::default()),
            entitlements: vec![Entitlement::LinuxVms],
            limits: Some(Limits {
                included_vm_seconds: Some(100 * 3600),
                ..Limits::default()
            }),
            usage: Usage {
                workspaces: vec![WorkspaceUsage {
                    workspace: "ws-build".into(),
                    vm_seconds: 3600,
                    ..WorkspaceUsage::default()
                }],
                ..Usage::default()
            },
            credit: Some("$12.40".into()),
            invoices: vec![
                Invoice {
                    id: "in_2".into(),
                    amount: "$24.10".into(),
                    status: InvoiceStatus::Open,
                    url: "https://tilo.sh/billing/invoices/in_2".into(),
                    ..Invoice::default()
                },
                Invoice {
                    id: "in_1".into(),
                    amount: "$20.00".into(),
                    status: InvoiceStatus::Paid,
                    url: "https://tilo.sh/billing/invoices/in_1".into(),
                    ..Invoice::default()
                },
            ],
        }),
        manage_billing: Some("https://tilo.sh/billing".into()),
    }
}

#[test]
fn someone_with_one_device_that_sells_nothing_sees_no_hosting() {
    en();
    let desk = TiloDesk {
        hosts: vec![HostAccount::sells_nothing("ada-laptop", HostKind::Device)],
        ..TiloDesk::default()
    };
    assert!(!hosting_relevant(&desk.hosts));
    let rows = rows(&desk);
    assert_eq!(captions(&rows), ["Account"]);
    assert_eq!(usage_line(&desk.hosts, "ws-build"), None);
}

#[test]
fn a_self_hosted_host_shows_and_offers_no_plans_or_billing() {
    en();
    let desk = TiloDesk {
        hosts: vec![HostAccount::sells_nothing(
            "Ada's server",
            HostKind::SelfHosted,
        )],
        ..TiloDesk::default()
    };
    let rows = rows(&desk);
    assert_eq!(captions(&rows), ["Hosts & compute", "Account"]);
    let host = items(&rows)[0];
    assert_eq!(host.title, "Ada's server");
    assert_eq!(facts(host), ["Your own host"]);
    assert_eq!(host.action, None, "nothing to manage billing of");
    assert!(
        !items(&rows)
            .iter()
            .any(|item| item.key.starts_with("overview-invoice")),
        "no invoices"
    );
}

/// Tilo Cloud is one row of facts: the plan, the hours used of those it includes and the
/// credit, with Manage billing. Invoices are Settings' Billing page's, not the overview's.
#[test]
fn tilo_cloud_states_its_plan_use_and_credit_with_manage_billing() {
    en();
    let desk = TiloDesk {
        hosts: vec![tilo_cloud()],
        ..TiloDesk::default()
    };
    let rows = rows(&desk);
    let items = items(&rows);
    let host = items[0];
    assert_eq!(host.title, "Tilo Cloud");
    assert_eq!(facts(host), ["Pro", "1 of 100 hr", "$12.40 credit"]);
    assert_eq!(
        host.action,
        Some((
            "Manage billing".to_owned(),
            OverviewPick::OpenPage("https://tilo.sh/billing".into())
        ))
    );
    assert!(
        !items
            .iter()
            .any(|item| item.key.starts_with("overview-invoice")),
        "no invoices in the overview"
    );
    assert_eq!(
        usage_line(&desk.hosts, "ws-build").as_deref(),
        Some("Tilo Cloud, 1 hr this month")
    );
}

#[test]
fn an_account_without_a_card_is_told_when_one_is_asked_for() {
    en();
    let mut host = tilo_cloud();
    host.standing = Some(Standing {
        account: "acct-1".into(),
        ..Standing::default()
    });
    let rows = rows(&TiloDesk {
        hosts: vec![host],
        ..TiloDesk::default()
    });
    assert_eq!(
        facts(items(&rows)[0]),
        ["No card yet: added the first time something needs a Linux machine"]
    );
}

#[test]
fn agents_let_in_can_each_be_revoked() {
    en();
    let desk = TiloDesk {
        door: Some(Door {
            grants: vec![Grant {
                id: "g1".into(),
                agent: "Claude Code".into(),
                runs: vec![Run {
                    name: "Claude Code #2".into(),
                    started_ms: 1,
                }],
                ..Grant::default()
            }],
            ..Door::default()
        }),
        ..TiloDesk::default()
    };
    let rows = rows(&desk);
    assert_eq!(captions(&rows), ["Agents", "Account"]);
    let agent = items(&rows)[0];
    assert_eq!(agent.title, "Claude Code");
    assert_eq!(facts(agent), ["All your workspaces", "working now"]);
    assert_eq!(
        agent.action,
        Some(("Revoke".to_owned(), OverviewPick::RevokeGrant("g1".into())))
    );
}

#[test]
fn the_account_holds_what_the_tilo_mark_offered() {
    en();
    let desk = TiloDesk {
        account: Some(AccountLine {
            name: "Ada".into(),
            detail: Some("ada@tilo.sh".into()),
        }),
        elsewhere: vec![("host-ws-3".into(), "Garden".into())],
        actions: vec![("session-lock".into(), "Lock".into())],
        feedback: true,
        ..TiloDesk::default()
    };
    let rows = rows(&desk);
    assert_eq!(captions(&rows), ["More workspaces", "Account"]);
    let picks: Vec<(&str, Option<&OverviewPick>)> = items(&rows)
        .iter()
        .map(|item| (item.title.as_str(), item.press.as_ref()))
        .collect();
    assert_eq!(
        picks,
        [
            (
                "Garden",
                Some(&OverviewPick::HostAction("host-ws-3".into()))
            ),
            ("Ada", Some(&OverviewPick::AccountSettings)),
            (
                "Lock",
                Some(&OverviewPick::HostAction("session-lock".into()))
            ),
            ("About Tilo", Some(&OverviewPick::AboutTilo)),
            ("Give feedback…", Some(&OverviewPick::Feedback)),
        ]
    );
}
