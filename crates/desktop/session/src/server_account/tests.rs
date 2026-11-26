use std::time::Duration;

use tilo_action_services::server_account::Entitlement;

use super::*;

const SELLING: &str = r#"{"sells":{"host":"Tilo Cloud",
    "plans":[{"id":"pro","name":"Pro","limits":{"max_running":2,"max_vms":5,"sizes":["small"]}}],
    "add_payment_method":"https://tilo.sh/billing/card?account={account}",
    "manage_billing":"https://tilo.sh/billing?account={account}"}}"#;

#[test]
fn a_device_sells_nothing() {
    let api = ServerAccountApi::of(Source::Device("ada-laptop".into()));
    let account = api.read();
    assert_eq!(
        account,
        ServerAccount::sells_nothing("ada-laptop", ServerKind::Device)
    );
    assert!(account.plans.is_empty() && account.subscription.is_none());
}

#[test]
fn a_hosts_answer_follows_its_plans_file_as_a_card_is_added() {
    let dir = tempfile::tempdir().unwrap();
    let plans = dir.path().join("plans.json");
    std::fs::write(&plans, SELLING).unwrap();
    let api = ServerAccountApi::of(Source::Hosted(ServerAccountSource {
        account: "acct-1".into(),
        plans: plans.clone(),
        log: None,
        socket: None,
        name: "unused".into(),
    }));
    let mut changes = api.changes.subscribe();
    // phase: no card on file
    {
        let account = api.read();
        assert_eq!(account.server, "Tilo Cloud");
        assert_eq!(
            account.manage_billing.as_deref(),
            Some("https://tilo.sh/billing?account=acct-1")
        );
        assert!(
            !account
                .subscription
                .unwrap()
                .entitled(Entitlement::LinuxVms)
        );
    }
    // phase: the provider names the account, and followers are told
    {
        let with_card = SELLING.replacen(
            r#"{"sells""#,
            r#"{"accounts":{"acct-1":{"max_running":2,"max_vms":5,"sizes":["small"],"plan":"pro"}},"sells""#,
            1,
        );
        tilo_files::native::atomic::write_creating_parent(&plans, with_card.as_bytes()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(10), changes.changed()).await
            })
            .expect("the change is told in time")
            .unwrap();
        let account = api.read();
        assert_eq!(account.plan().map(|plan| plan.name.as_str()), Some("Pro"));
        assert!(
            account
                .subscription
                .unwrap()
                .entitled(Entitlement::LinuxVms)
        );
    }
}
