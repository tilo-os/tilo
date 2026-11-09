use legacy_vm::hosted::meter::VmIdentity;
use legacy_vm::hosted::plan::Plan;
use legacy_vm::hosted::size::VmSize;
use tilo_actions::remote::host_account::{InvoiceStatus, PlanOffer};

use super::*;

fn selling() -> Plans {
    serde_json::from_str(
        r#"{"sells":{"host":"Tilo Cloud",
            "plans":[{"id":"pro","name":"Pro","price":"$20 a month",
                      "limits":{"max_running":2,"max_vms":5,"sizes":["small","default"],"included_vm_seconds":360000}}],
            "add_payment_method":"https://tilo.sh/billing/card?account={account}",
            "manage_billing":"https://tilo.sh/billing?account={account}"}}"#,
    )
    .unwrap()
}

fn record(workspace: &str, kind: UsageKind, end_ms: u64, seconds: u64) -> UsageRecord {
    UsageRecord {
        kind,
        identity: VmIdentity {
            vm: format!("{workspace}/default"),
            workspace: workspace.into(),
            environment: "default".into(),
            account: "acct-1".into(),
            size: VmSize::Small,
        },
        start_ms: end_ms.saturating_sub(seconds.saturating_mul(1000)),
        end_ms,
        millis: seconds.saturating_mul(1000),
        seconds,
        ended: None,
        snapshot_bytes: None,
    }
}

#[test]
fn a_host_that_sells_nothing_answers_no_plans_and_no_standing() {
    let account = account_of(
        &Plans::default(),
        "acct-1",
        "Ada's server",
        Usage::default(),
    );
    assert_eq!(
        account,
        HostAccount::sells_nothing("Ada's server", HostKind::SelfHosted)
    );
    assert!(!account.sells());
}

#[test]
fn an_account_without_a_card_sees_the_offers_and_no_entitlement() {
    let account = account_of(&selling(), "acct-1", "unused", Usage::default());
    assert_eq!(account.host, "Tilo Cloud");
    assert_eq!(account.kind, HostKind::Provider);
    assert_eq!(
        account
            .plans
            .iter()
            .map(|plan| plan.id.as_str())
            .collect::<Vec<_>>(),
        ["pro"]
    );
    let standing = account.standing.as_ref().unwrap();
    assert!(!standing.payment_method);
    assert!(!standing.entitled(Entitlement::LinuxVms));
    assert_eq!(standing.limits, None);
    assert_eq!(
        account.manage_billing.as_deref(),
        Some("https://tilo.sh/billing?account=acct-1")
    );
}

#[test]
fn an_account_with_a_card_has_its_plan_limits_credit_and_invoices() {
    let mut plans = selling();
    plans.accounts.insert(
        "acct-1".into(),
        Plan {
            max_running: 2,
            max_vms: 5,
            sizes: vec![VmSize::Small],
            plan: Some("pro".into()),
            credit: Some("$12.40 credit".into()),
            invoices: vec![tilo_actions::remote::host_account::Invoice {
                id: "in_1".into(),
                issued_ms: 1,
                amount: "$20.00".into(),
                status: InvoiceStatus::Paid,
                url: "https://tilo.sh/billing/invoices/in_1".into(),
            }],
            ..Plan::default()
        },
    );
    let account = account_of(&plans, "acct-1", "unused", Usage::default());
    assert_eq!(
        account.plan().map(|plan: &PlanOffer| plan.name.as_str()),
        Some("Pro")
    );
    let standing = account.standing.unwrap();
    assert!(standing.payment_method);
    assert!(standing.entitled(Entitlement::LinuxVms));
    let limits = standing.limits.unwrap();
    assert_eq!(
        (limits.max_running, limits.sizes, limits.included_vm_seconds),
        (2, vec!["small".to_string()], Some(360_000))
    );
    assert_eq!(standing.credit.as_deref(), Some("$12.40 credit"));
    assert_eq!(standing.invoices.len(), 1);
}

#[test]
fn usage_is_per_workspace_in_the_period_most_used_first() {
    let records = [
        record("a", UsageKind::Running, 1_500, 30),
        record("b", UsageKind::Running, 1_600, 90),
        record("a", UsageKind::Snapshot, 1_700, 400),
        record("a", UsageKind::Running, 1_800, 15),
        // ended before the period
        record("b", UsageKind::Running, 900, 1_000),
    ];
    let usage = usage_of(&records, &["c".into(), "a".into()], 1_000, 2_000);
    let rows: Vec<_> = usage
        .workspaces
        .iter()
        .map(|w| {
            (
                w.workspace.as_str(),
                w.vm_seconds,
                w.snapshot_seconds,
                w.running,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [("b", 90, 0, false), ("a", 45, 400, true), ("c", 0, 0, true)]
    );
    assert_eq!(usage.vm_seconds(), 135);
}

#[test]
fn the_period_is_the_providers_or_the_calendar_month() {
    // 2026-11-08T12:00:00Z
    let now = 1_794_139_200_000;
    let (from, to) = period(&selling(), "acct-1", now);
    // 2026-11-01 and 2026-12-01
    assert_eq!((from, to), (1_793_491_200_000, 1_796_083_200_000));
    let mut plans = selling();
    plans.accounts.insert(
        "acct-1".into(),
        Plan {
            period_start_ms: Some(1_793_923_200_000), // 2026-11-06
            ..Plan::default()
        },
    );
    assert_eq!(
        period(&plans, "acct-1", now),
        (1_793_923_200_000, 1_796_515_200_000) // to 2026-12-06
    );
}

#[test]
fn a_card_added_is_read_once_the_plans_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plans.json");
    std::fs::write(&path, serde_json::to_vec(&selling()).unwrap()).unwrap();
    let hosted = HostedAccount {
        account: "acct-1".into(),
        plans: path.clone(),
        socket: None,
        name: "unused".into(),
    };
    let (tell, changed) = std::sync::mpsc::channel();
    let _watch = hosted
        .watch(move || {
            let _ = tell.send(());
        })
        .unwrap();
    assert!(!hosted.read(0).standing.unwrap().payment_method);
    let mut plans = selling();
    plans.accounts.insert("acct-1".into(), Plan::default());
    tilo_files::native::atomic::write_creating_parent(&path, &serde_json::to_vec(&plans).unwrap())
        .unwrap();
    changed
        .recv_timeout(Duration::from_secs(10))
        .expect("the change is told");
    assert!(hosted.read(0).standing.unwrap().payment_method);
}
