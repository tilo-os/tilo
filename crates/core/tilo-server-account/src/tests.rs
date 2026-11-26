use linux_vm::hosted::meter::VmIdentity;
use linux_vm::hosted::plan::Plan;
use tilo_action_services::server_account::{InvoiceStatus, PlanOffer};
use tilo_server_protocol::hosted_vm::VmSize;

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

fn record(workspace: &str, kind: UsageKind, end_ms: u64, seconds: u64) -> UsageEntry {
    UsageEntry {
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
fn a_host_that_sells_nothing_answers_no_plans_and_no_subscription() {
    let account = account_of(
        &Plans::default(),
        "acct-1",
        "Ada's server",
        Usage::default(),
    );
    assert_eq!(account.server, "Ada's server");
    assert_eq!(account.kind, ServerKind::SelfHosted);
    assert!(account.plans.is_empty());
    assert!(account.subscription.is_none());
    assert!(account.manage_billing.is_none());
    assert!(!account.sells());
}

#[test]
fn an_account_without_a_card_sees_the_offers_and_no_entitlement() {
    let account = account_of(&selling(), "acct-1", "unused", Usage::default());
    assert_eq!(account.server, "Tilo Cloud");
    assert_eq!(account.kind, ServerKind::Provider);
    assert_eq!(
        account
            .plans
            .iter()
            .map(|plan| plan.id.as_str())
            .collect::<Vec<_>>(),
        ["pro"]
    );
    let subscription = account.subscription.as_ref().unwrap();
    assert!(subscription.payment_method.is_none());
    assert!(!subscription.entitled(Entitlement::LinuxVms));
    assert_eq!(subscription.limits, None);
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
            credit: Some("$12.40".into()),
            invoices: vec![tilo_action_services::server_account::Invoice {
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
    let subscription = account.subscription.unwrap();
    assert!(subscription.payment_method.is_some());
    assert!(subscription.entitled(Entitlement::LinuxVms));
    let limits = subscription.limits.unwrap();
    assert_eq!(
        (limits.max_running, limits.sizes, limits.included_vm_seconds),
        (2, vec!["small".to_string()], Some(360_000))
    );
    assert_eq!(subscription.credit.as_deref(), Some("$12.40"));
    assert_eq!(subscription.invoices.len(), 1);
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
    let hosted = ServerAccountSource {
        account: "acct-1".into(),
        plans: path.clone(),
        log: None,
        socket: None,
        name: "unused".into(),
    };
    let (tell, changed) = std::sync::mpsc::channel();
    let _watch = hosted
        .watch(move || {
            let _ = tell.send(());
        })
        .unwrap();
    assert!(
        hosted
            .read(0)
            .subscription
            .unwrap()
            .payment_method
            .is_none()
    );
    let mut plans = selling();
    plans.accounts.insert("acct-1".into(), Plan::default());
    tilo_files::native::atomic::write_creating_parent(&path, &serde_json::to_vec(&plans).unwrap())
        .unwrap();
    changed
        .recv_timeout(Duration::from_secs(10))
        .expect("the change is told");
    assert!(
        hosted
            .read(0)
            .subscription
            .unwrap()
            .payment_method
            .is_some()
    );
}

#[test]
fn the_subscription_shows_the_free_page_actions_and_the_count_of_the_period() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plans.json");
    std::fs::write(
        &path,
        br#"{"sells":{"host":"tilo.sh","plans":[],
             "add_payment_method":"https://tilo.sh/billing/card?account={account}",
             "browser_free_actions":100}}"#,
    )
    .unwrap();
    let log = dir.path().join("browser-actions.jsonl");
    let actions = browser_actions::Log::new(log.clone());
    // 2026-11-08T12:00:00Z, the calendar month's period
    let now = 1_794_139_200_000;
    for at_ms in [1_793_491_200_000, now, now + 1] {
        actions
            .record(&browser_actions::PageAction {
                account: "acct-1".into(),
                workspace: "ws1".into(),
                at_ms,
            })
            .unwrap();
        actions
            .record(&browser_actions::PageAction {
                account: "acct-2".into(),
                workspace: "ws1".into(),
                at_ms,
            })
            .unwrap();
    }
    let hosted = ServerAccountSource {
        account: "acct-1".into(),
        plans: path.clone(),
        log: Some(log),
        socket: None,
        name: "unused".into(),
    };
    // phase: no card: the allowance is all it may use, and its count
    {
        let account = hosted.read(now);
        let subscription = account.subscription.unwrap();
        assert!(subscription.payment_method.is_none());
        assert_eq!(
            subscription.limits,
            Some(Limits {
                free_browser_actions: Some(100),
                ..Limits::default()
            })
        );
        assert_eq!(subscription.usage.browser_actions, 3);
        assert_eq!(
            (
                subscription.usage.period_start_ms,
                subscription.usage.period_end_ms
            ),
            (1_793_491_200_000, 1_796_083_200_000)
        );
    }
    // phase: a card, on a plan, with the same allowance and count
    {
        let mut plans: Plans = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        plans.accounts.insert(
            "acct-1".into(),
            Plan {
                plan: Some("pro".into()),
                ..Plan::default()
            },
        );
        plans.sells.as_mut().unwrap().plans.push(PlanOffer {
            id: "pro".into(),
            name: "Pro".into(),
            ..PlanOffer::default()
        });
        std::fs::write(&path, serde_json::to_vec(&plans).unwrap()).unwrap();
        let subscription = hosted.read(now).subscription.unwrap();
        assert!(subscription.payment_method.is_some());
        assert_eq!(
            subscription
                .limits
                .and_then(|limits| limits.free_browser_actions),
            Some(100)
        );
        assert_eq!(subscription.usage.browser_actions, 3);
    }
}

/// A host that does not answer is told from one that answers nothing: a plans file
/// that does not read, or a hosted-VM socket named but not answering, is the host
/// being out of reach, not an account without usage.
#[test]
fn a_host_out_of_reach_is_told_from_one_that_answers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let plans = dir.path().join("plans.json");
    let now = 1_794_139_200_000;
    // A host that sells and answers.
    {
        std::fs::write(&plans, serde_json::to_vec(&selling()).unwrap()).unwrap();
        let hosted = ServerAccountSource {
            account: "acct-1".into(),
            plans: plans.clone(),
            log: None,
            socket: None,
            name: "unused".into(),
        };
        let account = hosted.try_read(now).expect("the host answers");
        assert!(account.sells());
        // No card and no plan: the host sells, this account signed up for none.
        assert!(
            account
                .subscription
                .as_ref()
                .is_some_and(|subscription| subscription.plan.is_none())
        );
    }
    // A plans file that does not read is the host out of reach.
    {
        std::fs::write(&plans, "{ not json").unwrap();
        let hosted = ServerAccountSource {
            account: "acct-1".into(),
            plans: plans.clone(),
            log: None,
            socket: None,
            name: "unused".into(),
        };
        let why = hosted.try_read(now).expect_err("the host is out of reach");
        assert!(why.contains("plans.json"), "{why}");
        // The tolerant read still answers what it can, for the callers that want that.
        assert!(hosted.read(now).plans.is_empty());
    }
    // A hosted-VM socket that does not answer is the host out of reach.
    {
        std::fs::write(&plans, serde_json::to_vec(&selling()).unwrap()).unwrap();
        // A socket under a path that is no directory: the host cannot be reached there.
        let socket = plans.join("hosted-vm.sock");
        let hosted = ServerAccountSource {
            account: "acct-1".into(),
            plans,
            log: None,
            socket: Some(socket),
            name: "unused".into(),
        };
        let why = hosted
            .try_read(now)
            .expect_err("the host's service is out of reach");
        assert!(why.contains("hosted-vm.sock"), "{why}");
    }
}
