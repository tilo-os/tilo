//! What Settings' billing page reads of the person's account with their host, state by
//! state, and what the page's package can name of it: every field the surface declares
//! is in the reading, and every state it names is one the account can be in.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use serde_json::Value;
use tilo_server_account::ServerAccountSource;

use super::*;

/// The surface the tilolang package is compiled against, as it is committed beside this
/// module.
const SURFACE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/host-account-surface.json"
));

/// A host that sells plans and manages billing, as its plans file writes it.
fn selling() -> &'static str {
    r#"{"sells":{"host":"Tilo Cloud",
        "plans":[{"id":"pro","name":"Pro","price":"$20 a month",
                  "limits":{"max_running":2,"max_vms":5,"sizes":["small"]}}],
        "add_payment_method":"https://tilo.sh/billing/card?account={account}",
        "manage_billing":"https://tilo.sh/billing?account={account}"}}"#
}

/// A file holding `plans`, and the account of it.
fn host(named: Option<&str>, plans: &str) -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().join("plans.json");
    std::fs::write(&path, plans).expect("the plans file");
    let plans = match named {
        None => plans.to_owned(),
        // The account's own entry, appended to what the host sells.
        Some(account) => {
            let mut plans: Value = serde_json::from_str(plans).expect("plans");
            if plans.get("accounts").is_none() {
                plans["accounts"] = serde_json::json!({});
            }
            plans["accounts"][account] = serde_json::json!({
                "max_running": 2,
                "max_vms": 5,
                "sizes": ["small"],
                "plan": "pro",
                "credit": "$12.40",
                "card": {"brand": "Visa", "last4": "4242"},
                "invoices": [
                    {"id": "in_1", "issued_ms": 1_763_000_000_000_u64,
                     "amount": "$24.10", "status": "paid",
                     "url": "https://tilo.sh/billing/invoices/in_1"},
                    {"id": "in_2", "issued_ms": 1_760_000_000_000_u64,
                     "amount": "$20.00", "status": "past-due",
                     "url": "javascript:alert(1)"},
                    {"id": "in_3", "amount": "$0.00", "status": "void",
                     "url": "https://tilo.sh/billing/invoices/in_3"}
                ]
            });
            plans.to_string()
        }
    };
    std::fs::write(&path, plans).expect("the plans file");
    let source = Source::Hosted(ServerAccountSource {
        account: "acct-1".into(),
        plans: path,
        log: None,
        socket: None,
        name: "This host".into(),
    });
    (dir, source)
}

/// The account of an account-less person on a host that sells.
#[test]
fn a_host_that_sells_an_account_nobody_signed_in_reads_as_signed_out() {
    let (_dir, source) = host(None, selling());
    let reading = reading_of(&source);
    assert_eq!(reading["state"], "signed-out", "{reading}");
    assert_eq!(reading["server"], "Tilo Cloud");
    assert!(
        reading["words"].as_str().unwrap().contains("not signed in"),
        "{reading}"
    );
    assert_eq!(reading["plan_name"], "");
    assert_eq!(reading["has_card"], false);
    // Where the person signs in is the provider's page, which the button opens.
    assert_eq!(
        reading["manage"], "https://tilo.sh/billing?account=acct-1",
        "{reading}"
    );
    assert_eq!(reading["invoices"], serde_json::json!([]));
}

/// The account on a plan, with a card, usage, credit and invoices.
#[test]
fn an_account_on_a_plan_reads_its_plan_card_usage_and_invoices() {
    let (_dir, source) = host(Some("acct-1"), selling());
    let reading = reading_of(&source);
    assert_eq!(reading["state"], "available", "{reading}");
    assert_eq!(reading["server"], "Tilo Cloud");
    assert_eq!(reading["plan_name"], "Pro");
    assert_eq!(reading["plan_price"], "$20 a month");
    assert_eq!(reading["has_card"], serde_json::json!(true));
    assert_eq!(reading["card_brand"], "Visa");
    assert_eq!(reading["card_last4"], "4242");
    assert_eq!(reading["credit"], "$12.40");
    assert_eq!(reading["usage_seconds"], 0);
    assert!(
        reading["included_seconds"].is_null(),
        "the plan sets no included seconds: {reading}"
    );
    let invoices = reading["invoices"].as_array().expect("invoices");
    assert_eq!(invoices.len(), 3);
    assert_eq!(invoices[0]["id"], "in_1");
    assert_eq!(invoices[0]["amount"], "$24.10");
    assert_eq!(invoices[0]["status"], "paid");
    assert_eq!(
        invoices[0]["page"], "https://tilo.sh/billing/invoices/in_1",
        "an invoice opens the provider's page"
    );
    assert!(
        invoices[0]["issued"]
            .as_str()
            .is_some_and(|issued| issued.starts_with("2025-")),
        "issued as ISO text: {invoices:?}"
    );
    assert_eq!(invoices[1]["status"], "past-due");
    assert_eq!(
        invoices[1]["page"], "",
        "a page that is not https is never opened"
    );
    assert!(
        invoices[2]["issued"].is_null(),
        "a provider that gave no issue time reads as none: {invoices:?}"
    );
}

/// This computer is nobody's host: the account reads as no account, in this device's
/// words.
#[test]
fn a_device_has_no_hosting_account_to_pay() {
    let source = Source::Device("Ada's laptop".into());
    let reading = reading_of(&source);
    assert_eq!(reading["state"], "no-account", "{reading}");
    assert_eq!(reading["server"], "Ada's laptop");
    assert!(
        reading["words"]
            .as_str()
            .unwrap()
            .contains("nothing to pay"),
        "{reading}"
    );
    assert_eq!(reading["manage"], "");
    assert_eq!(reading["invoices"], serde_json::json!([]));
}

/// A server that sells nothing says so, and names itself.
#[test]
fn a_server_that_sells_nothing_has_no_account_either() {
    let (_dir, source) = host(
        None,
        r#"{"default":{"max_running":4,"max_vms":3,"sizes":["small"]}}"#,
    );
    let reading = reading_of(&source);
    assert_eq!(reading["state"], "no-account", "{reading}");
    assert_eq!(reading["server"], "This host");
    assert_eq!(reading["plan_name"], "");
}

/// A host that cannot be asked is a state of its own, with its reasons, never an empty
/// account that looks like one nobody signed in for.
#[test]
fn a_host_out_of_reach_says_so() {
    let (dir, source) = host(None, "{ this does not read");
    let reading = reading_of(&source);
    assert_eq!(reading["state"], "unreachable", "{reading}");
    assert_eq!(reading["server"], "This host");
    assert!(
        reading["words"].as_str().unwrap().contains("plans.json"),
        "the host's own words: {reading}"
    );
    assert_eq!(reading["plan_name"], "");
    drop(dir);
}

/// Before the host answers the reading says it is reading: never an account that looks
/// new.
#[test]
fn the_reading_starts_by_saying_it_is_reading() {
    let (_dir, source) = host(None, selling());
    let mut served = Served::of(source);
    let before = served.read(RESOURCE).expect("the resource");
    assert_eq!(before["state"], "reading", "{before}");
    served.refresh();
    assert!(served.poll(), "the answer lands");
    let after = served.read(RESOURCE).expect("the resource");
    assert_eq!(after["state"], "signed-out", "{after}");
}

/// A host that could not be asked is asked again by itself, and one that answered is
/// not asked until it changes: the loop wakes at the retry, and a plain account waits.
#[test]
fn a_host_out_of_reach_is_asked_again_by_itself() {
    let (dir, source) = host(None, "{ this does not read");
    let mut served = Served::of(source);
    served.refresh();
    let _ = served.poll();
    let wait = served
        .wakes_in()
        .expect("an unreachable host is asked again");
    assert!(wait <= RETRY, "{wait:?}");
    drop(dir);
    let (_dir, source) = host(Some("acct-1"), selling());
    let mut answered = Served::of(source);
    answered.refresh();
    let _ = answered.poll();
    assert!(
        answered.wakes_in().is_none(),
        "an account that answered is asked only when it changes"
    );
}

/// Billing is the person's: Settings reads the account, and another app reads none of
/// it.
#[test]
fn only_settings_reads_the_account() {
    let (dir, source) = host(None, selling());
    let mut served = Served::of(source);
    served.refresh();
    let _ = served.poll();
    assert!(
        served
            .read_for(SETTINGS_APP, "ws1", RESOURCE)
            .is_some_and(|reading| reading["state"] == "signed-out")
    );
    for app in ["so.tilo.notes", "tilo.agent", "so.tilo.activity"] {
        assert_eq!(
            served.read_for(app, "ws1", RESOURCE),
            None,
            "{app} reads no billing"
        );
    }
    drop(dir);
}

/// Every field the page's package declares at the root is in the reading, and every
/// state and invoice status the package names is one this reading writes: the Rust side
/// and the tilolang surface are one contract.
#[test]
fn the_reading_is_what_the_packages_surface_declares() {
    let surface: Value = serde_json::from_str(SURFACE).expect("the surface reads");
    let resource = surface["resources"]
        .as_array()
        .and_then(|resources| resources.first())
        .expect("one resource");
    assert_eq!(resource["kind"], RESOURCE);
    let (_dir, source) = host(Some("acct-1"), selling());
    let reading = reading_of(&source);
    let fields = reading.as_object().expect("a record").clone();
    for field in resource["fields"].as_array().expect("fields") {
        let path = field["path"].as_str().expect("a path");
        assert!(
            fields.contains_key(path),
            "the surface declares `{path}`, and the reading holds {:?}",
            fields.keys().collect::<Vec<_>>()
        );
    }
    // What a field of a nested record holds is read the same way.
    let invoice = reading["invoices"]
        .as_array()
        .and_then(|invoices| invoices.first())
        .expect("an invoice");
    let invoice_type = resource["types"]
        .as_array()
        .expect("types")
        .iter()
        .find(|ty| ty["name"] == "HostInvoice")
        .expect("HostInvoice");
    for field in invoice_type["fields"].as_array().expect("fields") {
        let name = field["name"].as_str().expect("a name");
        assert!(
            invoice.get(name).is_some(),
            "an invoice declares `{name}`, and the reading holds {:?}",
            invoice
                .as_object()
                .map(|invoice| invoice.keys().collect::<Vec<_>>())
        );
    }
    // The states the package names are the ones the reading can be in.
    let cases = |name: &str| -> Vec<String> {
        resource["types"]
            .as_array()
            .expect("types")
            .iter()
            .find(|ty| ty["name"] == name)
            .expect("a declared enum")["cases"]
            .as_array()
            .expect("cases")
            .iter()
            .map(|case| case["name"].as_str().expect("a case").to_owned())
            .collect()
    };
    let states = cases("HostAccountState");
    for state in [
        State::Reading,
        State::Available,
        State::SignedOut,
        State::NoAccount,
        State::Unreachable,
    ] {
        assert!(
            states.contains(&state.word().to_owned()),
            "{state:?}: {states:?}"
        );
    }
    let statuses = cases("HostInvoiceStatus");
    for status in [
        InvoiceStatus::Paid,
        InvoiceStatus::Open,
        InvoiceStatus::PastDue,
        InvoiceStatus::Void,
    ] {
        assert!(
            statuses.contains(&status_word(status).to_owned()),
            "{status:?}: {statuses:?}"
        );
    }
}

/// The person's Tilo as the shell reads it: the workspaces their host offers elsewhere, and the
/// host running their workspaces with the plan, what it used and its credit.
#[test]
fn the_persons_tilo_names_the_workspaces_elsewhere_and_the_host() {
    let (_dir, source) = host(Some("acct-1"), selling());
    let mut served = Served::with(
        source,
        std::rc::Rc::new(|| {
            vec![Elsewhere {
                action: "open-workspace:https://tilo.sh/abc".to_owned(),
                name: "Tilo".to_owned(),
            }]
        }),
    );
    served.refresh();
    let _ = served.poll();
    let reading = served
        .read_for(SHELL_APP, "ws1", YOURS_RESOURCE)
        .expect("the shell reads the person's Tilo");
    let yours: Yours = serde_json::from_value(reading).expect("the person's Tilo reads");
    assert_eq!(
        yours.elsewhere,
        [Elsewhere {
            action: "open-workspace:https://tilo.sh/abc".to_owned(),
            name: "Tilo".to_owned(),
        }],
        "a workspace elsewhere is named as its host names it"
    );
    let host = yours
        .hosts
        .first()
        .expect("the host running their workspaces");
    assert_eq!(host.server, "Tilo Cloud");
    assert_eq!(host.kind, "provider");
    assert_eq!(host.state, "available");
    assert_eq!(host.plan_name, "Pro");
    assert_eq!(host.plan_price, "$20 a month");
    assert!(host.has_card, "the account has a card on file");
    assert_eq!(host.credit, "$12.40");
    assert_eq!(host.manage, "https://tilo.sh/billing?account=acct-1");
}

/// What the account's Linux machines used is the host's count, and the plan's inclusion its own:
/// the overview reads both, so a plan says "1 hr of 100 hr" from these two numbers.
#[test]
fn a_plans_usage_and_inclusion_are_the_accounts_own_numbers() {
    use tilo_action_services::server_account::{
        Limits, PlanOffer, Subscription, Usage, WorkspaceUsage,
    };

    let included = 360_000_u64;
    let account = ServerAccount {
        server: "Tilo Cloud".to_owned(),
        kind: ServerKind::Provider,
        plans: vec![PlanOffer {
            id: "pro".to_owned(),
            name: "Pro".to_owned(),
            price: Some("$20 a month".to_owned()),
            limits: Limits {
                included_vm_seconds: Some(included),
                ..Limits::default()
            },
        }],
        subscription: Some(Subscription {
            account: "acct-1".to_owned(),
            plan: Some("pro".to_owned()),
            limits: Some(Limits {
                included_vm_seconds: Some(included),
                ..Limits::default()
            }),
            usage: Usage {
                workspaces: vec![WorkspaceUsage {
                    workspace: "ws-build".to_owned(),
                    vm_seconds: 3600,
                    ..WorkspaceUsage::default()
                }],
                ..Usage::default()
            },
            ..Subscription::default()
        }),
        manage_billing: Some("https://tilo.sh/billing".to_owned()),
    };
    let host: Host =
        serde_json::from_value(account_reading(&account)).expect("the host reads as a row");
    assert_eq!(host.usage_seconds, 3600, "an hour ran this period");
    assert_eq!(host.included_seconds, Some(360_000), "the plan's hour");
    assert_eq!(host.plan_name, "Pro");
    assert_eq!(host.credit, "");
}

/// One computer that runs its workspaces itself has no hosting to show: the section stands empty
/// rather than as a host with nothing on it.
#[test]
fn a_computer_that_sells_nothing_leaves_no_hosting_to_show() {
    let mut served = Served::of(Source::Device("Ada's laptop".to_owned()));
    served.refresh();
    let _ = served.poll();
    let reading = served
        .read_for(SHELL_APP, "ws1", YOURS_RESOURCE)
        .expect("the shell reads the person's Tilo");
    let yours: Yours = serde_json::from_value(reading).expect("the person's Tilo reads");
    assert!(yours.hosts.is_empty(), "{yours:?}");
    assert!(yours.elsewhere.is_empty());
}

/// A host that could not be reached stays a row of its own on the overview, saying so: never an
/// account that vanished, and never an empty section.
#[test]
fn a_host_out_of_reach_shows_as_unreachable() {
    let (dir, source) = host(None, "{ this does not read");
    let mut served = Served::of(source);
    served.refresh();
    let _ = served.poll();
    let reading = served
        .read_for(SHELL_APP, "ws1", YOURS_RESOURCE)
        .expect("the shell reads the person's Tilo");
    let yours: Yours = serde_json::from_value(reading).expect("the person's Tilo reads");
    let host = yours
        .hosts
        .first()
        .expect("a host that could not be reached is still a row");
    assert_eq!(host.state, "unreachable");
    assert!(
        host.words.contains("plans.json"),
        "the host's own words: {host:?}"
    );
    drop(dir);
}

/// The overview of the person's Tilo is the shell's: no other app reads their plan, usage or
/// credit from it.
#[test]
fn only_the_shell_reads_the_persons_tilo() {
    let (_dir, source) = host(Some("acct-1"), selling());
    let mut served = Served::of(source);
    served.refresh();
    let _ = served.poll();
    assert!(
        served
            .read_for(SHELL_APP, "ws1", YOURS_RESOURCE)
            .is_some_and(|reading| reading["hosts"].as_array().is_some_and(|h| h.len() == 1))
    );
    for app in ["so.tilo.notes", "tilo.agent", "so.tilo.settings"] {
        assert_eq!(
            served.read_for(app, "ws1", YOURS_RESOURCE),
            None,
            "{app} reads no part of the person's Tilo"
        );
    }
}
