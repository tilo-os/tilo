//! Settings' Billing page, over the host account the session publishes: the plan, the
//! payment method, what the Linux machines used, the invoices and the pages the provider
//! serves, in each state the host account can be in.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilolang_runtime::files::{Entry, FileTarget, FilesHost, TrashEntry};
use tilolang_runtime::{Actor, RefValue, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

/// Where the session publishes the person's account with their host.
fn account() -> ResourceRef {
    ResourceRef::parse_id("host-account", "tilo.host-account#HostAccount").expect("a resource id")
}

/// The pages the app asked the shell to open.
#[derive(Clone, Default)]
struct Opened(Rc<RefCell<Vec<String>>>);

impl FilesHost for Opened {
    fn list(&self, _path: &str) -> Result<Vec<Entry>, String> {
        Err("this test reads no disk".to_owned())
    }

    fn stat(&self, _path: &str) -> Result<Option<Entry>, String> {
        Err("this test reads no disk".to_owned())
    }

    fn read(&self, _path: &str) -> Result<Vec<u8>, String> {
        Err("this test reads no disk".to_owned())
    }

    fn watch(&self, _path: &str, _run: &str) -> Result<String, String> {
        Err("this test watches nothing".to_owned())
    }

    fn trash(&self, _path: &str) -> Result<TrashEntry, String> {
        Err("this test trashes nothing".to_owned())
    }

    fn open(&self, _file: FileTarget<'_>) -> Result<(), String> {
        Err("this test opens no file".to_owned())
    }

    fn open_link(&self, url: &str) -> Result<(), String> {
        self.0.borrow_mut().push(url.to_owned());
        Ok(())
    }
}

/// The app as it ships, reading `reading` as the person's host account.
struct Installed {
    host: Host,
    opened: Opened,
}

impl Installed {
    fn new(reading: &str) -> Self {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
        let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
        let ids = tilolang::IdsLock::parse(
            &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
        )
        .expect("its ids parse");
        let metas = tilolang_tilo::platform_apps::platform_packages()
            .expect("the platform's packages read");
        let compiled =
            tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
        let host = Host::new(
            Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            || None,
        )
        .expect("a host");
        host.load_compiled(&compiled).expect("the package loads");
        tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
        host.runtime()
            .set_reading(&account(), reading)
            .expect("the account is read");
        let opened = Opened::default();
        host.runtime().set_device_files(opened.clone());
        Self { host, opened }
    }

    /// Open the page `pane` through the window's own action, as its list and search do, and
    /// draw what the page shows.
    fn open(&self, pane: &str) -> ViewInstance {
        let id = self
            .host
            .package(APP)
            .expect("the app is loaded")
            .meta
            .actions
            .iter()
            .find(|action| action.name == "open_pane")
            .expect("the app publishes `open_pane`")
            .id
            .clone();
        let me = Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
        self.host
            .runtime()
            .invoke_action(&me, (id.as_str(), vec![Value::text(pane)]))
            .unwrap_or_else(|why| panic!("open_pane({pane}): {why}"));
        let name = format!("{pane}_pane");
        let view = self
            .host
            .package(APP)
            .expect("the app is loaded")
            .meta
            .views
            .iter()
            .find(|view| view.name == name)
            .unwrap_or_else(|| panic!("the app declares no `{name}`"))
            .id
            .clone();
        let mut view = self
            .host
            .runtime()
            .open_view(&me, &view, Vec::new())
            .expect("the pane opens");
        view.set_lazy_rows(false).expect("the pane draws its rows");
        view.poll_patches().expect("the pane draws");
        view
    }

    /// The page the window shows, as the app's own document holds it.
    fn shown_pane(&self) -> String {
        let root = self.host.root_of(APP).expect("the app's root");
        match self.host.runtime().read(&root, "pane").expect("a pane") {
            Value::Text(text) => text,
            other => panic!("not text: {other:?}"),
        }
    }

    fn opened(&self) -> Vec<String> {
        self.opened.0.borrow().clone()
    }
}

/// The text a prop holds, an optional one's included.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::Text(text) => Some(text.clone()),
        Value::Rich(rich) => Some(rich.written_text()),
        Value::Opt(Some(inner)) => text_of(inner),
        _ => None,
    }
}

/// The text the pane draws, every node's.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(text_of)
        .collect()
}

/// The key of the control of `kind` whose label `label` reads.
fn labelled(tree: &UiNode, kind: &str, label: &str) -> String {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("a {kind} labelled {label}"))
        .key
        .clone()
}

/// The row whose title is `title`, as the pane lays it out.
fn row_titled(tree: &UiNode, title: &str) -> String {
    tree.of_kind("ui.ListRow")
        .into_iter()
        .find(|node| {
            ["title", "label", "value"]
                .iter()
                .any(|prop| node.props.get(*prop) == Some(&Value::text(title)))
        })
        .unwrap_or_else(|| panic!("a row titled {title}"))
        .key
        .clone()
}

/// An account on a plan, with a card, usage, credit and invoices whose pages the provider
/// serves over https.
const ON_A_PLAN: &str = r#"{
    "state": "available",
    "server": "Tilo Cloud",
    "words": "",
    "plan_name": "Pro",
    "plan_price": "$20 a month",
    "has_card": true,
    "card_brand": "Visa",
    "card_last4": "4242",
    "usage_seconds": 12000,
    "included_seconds": 360000,
    "credit": "$12.40",
    "manage": "https://tilo.sh/billing?account=acct-1",
    "invoices": [
        {"id": "in_1", "amount": "$24.10", "status": "paid",
         "issued": "2025-11-13T16:53:20Z",
         "page": "https://tilo.sh/billing/invoices/in_1"},
        {"id": "in_2", "amount": "$20.00", "status": "past-due",
         "issued": null,
         "page": "https://tilo.sh/billing/invoices/in_2"}
    ]
}"#;

/// The page shows the plan, the card, what the machines used, the credit and every
/// invoice the provider issued.
#[test]
fn the_billing_page_shows_the_plan_card_usage_and_invoices() {
    let app = Installed::new(ON_A_PLAN);
    let view = app.open("billing");
    assert_eq!(
        app.shown_pane(),
        "billing",
        "the window shows the Billing page"
    );
    let shown = texts(&view);
    for wanted in [
        "Tilo Cloud",
        "Pro, $20 a month",
        "Visa ·· 4242",
        "3 hr 20 min of 100 hr",
        "$12.40",
        "$24.10",
        "$20.00",
        "Paid, issued",
        "Past due",
    ] {
        assert!(
            shown.iter().any(|text| text.contains(wanted)),
            "the pane says `{wanted}`: {shown:?}"
        );
    }
    for label in [
        "Plan",
        "Card",
        "Linux machines this month",
        "Credit",
        "Invoices",
    ] {
        assert!(
            shown.iter().any(|text| text == label),
            "the pane labels `{label}`: {shown:?}"
        );
    }
}

/// The manage button opens the provider's billing page: payment and plan changes stay
/// there, never in a form Tilo draws.
#[test]
fn the_manage_button_opens_the_providers_page() {
    let app = Installed::new(ON_A_PLAN);
    let view = app.open("billing");
    view.press(&labelled(view.tree(), "ui.Button", "Manage billing"))
        .expect("the button runs");
    assert_eq!(
        app.opened(),
        ["https://tilo.sh/billing?account=acct-1"],
        "the provider's own page"
    );
}

/// Each invoice opens its own page on the provider's side.
#[test]
fn an_invoice_opens_its_own_page() {
    let app = Installed::new(ON_A_PLAN);
    let view = app.open("billing");
    view.fire(&row_titled(view.tree(), "$24.10"), "activate")
        .expect("the row opens its invoice");
    view.fire(&row_titled(view.tree(), "$20.00"), "activate")
        .expect("the row opens its invoice");
    assert_eq!(
        app.opened(),
        [
            "https://tilo.sh/billing/invoices/in_1",
            "https://tilo.sh/billing/invoices/in_2"
        ]
    );
}

/// Only the account's own pages open: an invoice id the account does not hold opens
/// nothing, so an app or an agent reaching the action opens no page of its own choosing.
#[test]
fn an_invoice_the_account_does_not_hold_opens_nothing() {
    let app = Installed::new(ON_A_PLAN);
    let _view = app.open("billing");
    let id = app
        .host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .actions
        .iter()
        .find(|action| action.name == "open_invoice")
        .expect("the app publishes `open_invoice`")
        .id
        .clone();
    let me = Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
    let account = Value::Ref(RefValue {
        target: Rc::new(account()),
        author: None,
    });
    app.host
        .runtime()
        .invoke_action(
            &me,
            (id.as_str(), vec![account.clone(), Value::text("in_9")]),
        )
        .expect("the action runs, and opens nothing");
    assert!(app.opened().is_empty(), "nothing opens: {:?}", app.opened());
    // The invoice the account holds opens the page the account names for it.
    app.host
        .runtime()
        .invoke_action(&me, (id.as_str(), vec![account, Value::text("in_2")]))
        .expect("the action runs");
    assert_eq!(
        app.opened(),
        ["https://tilo.sh/billing/invoices/in_2"],
        "the account's own page, by its id"
    );
}

/// A host that cannot be asked says so, with its reason, and shows no empty plan in its
/// place: an account nobody can read is not an account nobody signed in for.
#[test]
fn a_host_out_of_reach_says_so_on_the_page() {
    let app = Installed::new(
        r#"{
            "state": "unreachable",
            "server": "Tilo Cloud",
            "words": "Your host did not answer: plans.json: expected value at line 1",
            "plan_name": "", "plan_price": "", "has_card": false,
            "card_brand": "", "card_last4": "", "usage_seconds": 0,
            "included_seconds": null, "credit": "", "manage": "",
            "invoices": []
        }"#,
    );
    let view = app.open("billing");
    let shown = texts(&view);
    assert!(
        shown
            .iter()
            .any(|text| text.contains("plans.json: expected value at line 1")),
        "the host's own reason: {shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text.contains("No plan yet")),
        "no account is drawn in place of one that could not be read: {shown:?}"
    );
    assert!(
        !shown
            .iter()
            .any(|text| text == "Plan" || text == "Invoices"),
        "nothing is claimed about the account: {shown:?}"
    );
}

/// Signed out of a host that sells: the page says so and offers the provider's page where
/// the person signs in.
#[test]
fn a_signed_out_account_offers_the_provider_page() {
    let app = Installed::new(
        r#"{
            "state": "signed-out",
            "server": "Tilo Cloud",
            "words": "You are not signed in to this host yet: sign in on its page to see the plan and invoices.",
            "plan_name": "", "plan_price": "", "has_card": false,
            "card_brand": "", "card_last4": "", "usage_seconds": 0,
            "included_seconds": null, "credit": "", "manage": "https://tilo.sh/billing?account=acct-1",
            "invoices": []
        }"#,
    );
    let view = app.open("billing");
    let shown = texts(&view);
    assert!(
        shown.iter().any(|text| text.contains("not signed in")),
        "{shown:?}"
    );
    view.press(&labelled(view.tree(), "ui.Button", "Add a card"))
        .expect("the button runs");
    assert_eq!(app.opened(), ["https://tilo.sh/billing?account=acct-1"]);
}

/// This computer is nobody's host: the page says there is nothing to pay for, and claims
/// no plan.
#[test]
fn a_device_says_there_is_nothing_to_pay_for() {
    let app = Installed::new(
        r#"{
            "state": "no-account",
            "server": "Ada's laptop",
            "words": "Your workspaces run on this server itself, so there is nothing to pay for.",
            "plan_name": "", "plan_price": "", "has_card": false,
            "card_brand": "", "card_last4": "", "usage_seconds": 0,
            "included_seconds": null, "credit": "", "manage": "",
            "invoices": []
        }"#,
    );
    let view = app.open("billing");
    let shown = texts(&view);
    assert!(
        shown.iter().any(|text| text.contains("Ada's laptop")),
        "{shown:?}"
    );
    assert!(
        shown.iter().any(|text| text.contains("nothing to pay for")),
        "{shown:?}"
    );
    assert!(
        view.tree().of_kind("ui.Button").is_empty(),
        "there is no page to offer: {shown:?}"
    );
    assert!(
        !shown
            .iter()
            .any(|text| text == "Plan" || text == "Invoices"),
        "{shown:?}"
    );
}

/// While the host has not answered, the page says it is reading the account rather than
/// showing one with no plan.
#[test]
fn a_reading_account_says_it_is_reading() {
    let app = Installed::new(
        r#"{
            "state": "reading",
            "server": "Tilo Cloud",
            "words": "",
            "plan_name": "", "plan_price": "", "has_card": false,
            "card_brand": "", "card_last4": "", "usage_seconds": 0,
            "included_seconds": null, "credit": "", "manage": "",
            "invoices": []
        }"#,
    );
    let view = app.open("billing");
    let shown = texts(&view);
    assert!(
        shown
            .iter()
            .any(|text| text.contains("Reading your account")),
        "{shown:?}"
    );
    assert!(
        !shown
            .iter()
            .any(|text| text == "Plan" || text == "Invoices"),
        "{shown:?}"
    );
}
