//! Passwords keeps every secret in the workspace's vault and reaches it through the vault's own
//! calls (`tilo.vault/vault#…`): its records name an item there and hold no secret. A secret the
//! person types is named by where it is, never what it is; one the vault shows comes back under a
//! name only a view no capture reaches reads. A workspace an older Passwords kept plaintext in has
//! each secret added to the vault once, and the record emptied, the first time the vault settles.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, Value};
use tilolang_tilo::call::Callees;
use tilolang_tilo::host::Host;
use tilolang_tilo::task::Answered;

use crate::fixtures::{Replica, Workspace};

const BEFORE: (&str, &str) = (
    include_str!("passwords_before/app.tl"),
    include_str!("passwords_before/app.ids"),
);
const PASSWORDS: (&str, &str) = (
    include_str!("../../../../apps/tl/passwords/app.tl"),
    include_str!("../../../../apps/tl/passwords/app.ids"),
);
const APP: &str = "so.tilo.passwords";
const PASSWORD: &str = "correct horse battery staple";
const BANK: &str = "0123456789abcdef0123456789abcdef";

/// The vault as this test keeps it: what Passwords asked of it, by verb and its arguments, and
/// the items it lists.
#[derive(Default)]
struct Vault {
    asked: RefCell<Vec<(String, Json)>>,
    items: RefCell<Vec<Json>>,
}

/// What answers Passwords' calls of the vault, as the action registry would.
struct Answering(Rc<Vault>, Callees);

impl Invoker for Answering {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let Invocation::Action {
            app, name, args, ..
        } = invocation
        else {
            return Invoked::Refused("this test answers only calls".to_owned());
        };
        assert_eq!(
            (app.as_str(), name.as_str()),
            ("tilo.actions", "call"),
            "Passwords reaches the vault through its own calls alone"
        );
        let arg = |wanted: &str| {
            args.iter()
                .find(|(arg, _)| arg == wanted)
                .map(|(_, value)| value.clone())
        };
        let Some(Value::Text(address)) = arg("address") else {
            panic!("a call names its address: {args:?}");
        };
        let arguments = match arg("arguments") {
            Some(Value::Json(arguments)) => *arguments,
            other => panic!("a call's arguments are a document: {other:?}"),
        };
        let verb = address
            .strip_prefix("tilo.vault/vault#")
            .unwrap_or_else(|| panic!("Passwords calls the vault alone: {address}"))
            .to_owned();
        self.0
            .asked
            .borrow_mut()
            .push((verb.clone(), arguments.clone()));
        let output = match verb.as_str() {
            "list" => json!({"items": *self.0.items.borrow()}),
            "add" => {
                let id = arguments["item"]
                    .as_str()
                    .and_then(|item| item.strip_prefix("vault.item:"))
                    .unwrap_or(BANK)
                    .to_owned();
                let view = json!({
                    "reference": format!("vault.item:{id}"),
                    "kind": arguments["kind"],
                    "title": arguments["title"],
                    "belongsTo": arguments.get("belongsTo").cloned().unwrap_or_else(|| json!([])),
                    "addedAt": 1,
                    "changedAt": 1,
                });
                let mut items = self.0.items.borrow_mut();
                items.retain(|item| item["reference"] != view["reference"]);
                items.push(view.clone());
                view
            }
            "reveal" => json!({"value": "shown:made-up-token"}),
            _ => json!({}),
        };
        let record = json!({
            "id": "inv-1",
            "status": "ended",
            "started_ms": 1,
            "output": output,
        });
        self.1
            .answered(invocation, Answered::Done(json!({"value": record})))
    }
}

impl Vault {
    /// The calls of `verb` so far, by their arguments.
    fn of(&self, verb: &str) -> Vec<Json> {
        self.asked
            .borrow()
            .iter()
            .filter(|(asked, _)| asked == verb)
            .map(|(_, arguments)| arguments.clone())
            .collect()
    }
}

fn action_id(host: &Host, name: &str) -> String {
    host.package(APP)
        .expect("the Passwords package")
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("Passwords has {name}"))
        .id
        .clone()
}

/// Run Passwords' action `name` as the person's own call, which they approved on the system's
/// confirm: the run is the one they answered, so what Passwords' own code calls of its own runs on
/// that yes and does not ask again.
fn run(replica: &Replica, name: &str, args: Vec<Value>) -> Value {
    let outcome = replica
        .host
        .runtime()
        .invoke_approved_through(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            None,
            (action_id(&replica.host, name).as_str(), args),
            None,
        )
        .unwrap_or_else(|why| panic!("{name}: {why:?}"));
    replica.commit();
    match outcome {
        InvokeOutcome::Done(value) => value,
        other => panic!("{name} runs: {other:?}"),
    }
}

/// The field `name` of `at`, as the Passwords that runs declares it.
fn field(host: &Host, at: &ResourceRef, name: &str) -> Value {
    let package = host.package(APP).expect("the Passwords package");
    let declared = package
        .meta
        .types
        .iter()
        .find(|t| t.id == at.ty)
        .expect("the type");
    let field = declared
        .fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("the type declares {name}"));
    host.runtime().read(at, &field.id).expect("the field")
}

fn text(host: &Host, at: &ResourceRef, name: &str) -> String {
    match field(host, at, name) {
        Value::Text(text) => text,
        other => panic!("{name} is text: {other:?}"),
    }
}

fn items(host: &Host) -> Vec<ResourceRef> {
    let root = host.root_of(APP).expect("the root");
    match field(host, &root, "items") {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(reference) => Some((*reference.target).clone()),
                _ => None,
            })
            .collect(),
        other => panic!("items is a collection: {other:?}"),
    }
}

/// Every field of the item a record keeps, as text: where a secret would show.
fn record_words(host: &Host, item: &ResourceRef) -> String {
    [
        "title", "entry", "username", "password", "notes", "origins", "fields", "tags", "places",
        "labels",
    ]
    .iter()
    .map(|name| format!("{:?}", field(host, item, name)))
    .collect::<Vec<_>>()
    .join(" ")
}

/// A Passwords over `doc`, its calls of the vault answered by a vault this test keeps.
fn passwords(doc: Workspace) -> (Replica, Rc<Vault>) {
    let (replica, _) = Replica::new("ada", doc, &[PASSWORDS]);
    let vault = Rc::new(Vault::default());
    // The registry, and the invocation record a call answers with, as the dispatch declares them.
    let mut callees = Callees::default();
    let actions = tilolang::actions_meta();
    let invocations = tilolang_tilo::platform_apps::platform_package("tilo.invocations")
        .expect("the invocations' surface reads");
    callees.apps.insert(actions.app.clone(), actions);
    callees.apps.insert(invocations.app.clone(), invocations);
    replica
        .host
        .runtime()
        .set_foreign_invoker(Answering(Rc::clone(&vault), callees));
    (replica, vault)
}

#[test]
fn an_older_passwords_plaintext_moves_into_the_vault_once_and_leaves_the_record() {
    // An older Passwords kept a login's password in its record.
    let (before, _) = Replica::new("ada", Workspace::new(), &[BEFORE]);
    let root = before.host.root_of(APP).expect("the root");
    run(
        &before,
        "save_login",
        vec![
            Value::resource(&root),
            Value::text("Bank"),
            Value::text("https://bank.example"),
            Value::text("ada"),
            Value::text(PASSWORD),
        ],
    );

    // Today's Passwords opens the same workspace, and the vault settles.
    let (today, vault) = passwords(before.doc.fork());
    let root = today.host.root_of(APP).expect("the root");
    let [bank] = &items(&today.host)[..] else {
        panic!("the older record is there");
    };
    assert!(record_words(&today.host, bank).contains(PASSWORD));
    run(&today, "settle", vec![Value::resource(&root)]);

    let added = vault.of("add");
    let [add] = &added[..] else {
        panic!("one item is added: {added:?}");
    };
    assert_eq!(add["secret"], PASSWORD);
    assert_eq!(add["username"], "ada");
    assert_eq!(add["title"], "Bank");
    assert_eq!(add["belongsTo"], json!(["https://bank.example"]));
    assert_eq!(
        text(&today.host, bank, "entry"),
        BANK,
        "the record names the vault's item: {:?}",
        field(&today.host, &root, "notice")
    );
    let left = record_words(&today.host, bank);
    assert!(
        !left.contains(PASSWORD),
        "the record holds no secret: {left}"
    );
    assert_eq!(text(&today.host, bank, "username"), "");
    assert!(
        left.contains("https://bank.example"),
        "where it signs in is what the vault lists: {left}"
    );

    // The vault lists the item now: settling again adds nothing more.
    run(&today, "settle", vec![Value::resource(&root)]);
    assert_eq!(vault.of("add").len(), 1, "an item is added once");
    assert_eq!(items(&today.host).len(), 1, "and listed once");
}

#[test]
fn a_new_item_is_the_vaults_and_its_record_holds_no_secret() {
    let (passwords, vault) = passwords(Workspace::new());
    let root = passwords.host.root_of(APP).expect("the root");
    run(
        &passwords,
        "new_item",
        vec![Value::resource(&root), Value::text("Mail")],
    );
    let added = vault.of("add");
    let [add] = &added[..] else {
        panic!("the item is added to the vault: {added:?}");
    };
    assert_eq!(add["title"], "Mail");
    assert_eq!(
        add["generate"],
        json!({}),
        "its password is drawn where the vault is"
    );
    assert!(add.get("secret").is_none(), "{add}");
    let [mail] = &items(&passwords.host)[..] else {
        panic!("one item");
    };
    assert_eq!(text(&passwords.host, mail, "entry"), BANK);

    // An item kept elsewhere (a mail account's password, another device's login) gets a record,
    // which holds what the vault lists of it alone.
    vault.items.borrow_mut().push(json!({
        "reference": "vault.item:fedcba9876543210fedcba9876543210",
        "kind": "key",
        "title": "Model key",
        "belongsTo": ["https://models.example"],
        "tags": ["work"],
        "fields": ["account"],
        "addedAt": 1,
        "changedAt": 1,
        "concerns": ["stale"],
    }));
    run(&passwords, "settle", vec![Value::resource(&root)]);
    let listed = items(&passwords.host);
    assert_eq!(listed.len(), 2, "the item kept elsewhere is listed");
    let key = listed
        .iter()
        .find(|item| text(&passwords.host, item, "title") == "Model key")
        .expect("the key's item");
    let words = record_words(&passwords.host, key);
    assert!(words.contains("work"), "its tags are searched: {words}");
    assert!(words.contains("account"), "its fields' labels: {words}");

    // The person changes its password on the card: the vault is told where it is, never what.
    run(&passwords, "change_secret", vec![Value::resource(mail)]);
    let changed = vault.of("add");
    let change = changed.last().expect("the change");
    assert_eq!(change["item"], format!("vault.item:{BANK}"));
    assert_eq!(change["secret"], json!({"$typed": 0}));

    // Copying and showing are the vault's: one puts the secret on the clipboard where the vault
    // is, the other answers it under a name only a protected view reads.
    run(&passwords, "copy_secret", vec![Value::resource(mail)]);
    assert_eq!(
        vault.of("copy"),
        [json!({"item": format!("vault.item:{BANK}"), "part": "secret"})]
    );
    let shown = run(&passwords, "reveal", vec![Value::resource(mail)]);
    let shown = format!("{shown:?}");
    assert!(shown.contains("shown:made-up-token"), "{shown}");
    let revealed = vault.of("reveal");
    assert!(
        revealed
            .iter()
            .all(|asked| asked["$show"] == true && asked["item"] == format!("vault.item:{BANK}")),
        "{revealed:?}"
    );
    let words = record_words(&passwords.host, mail);
    assert!(!words.contains("shown:"), "nothing shown is kept: {words}");

    // An item the vault no longer keeps leaves the list.
    vault
        .items
        .borrow_mut()
        .retain(|item| item["title"] != "Model key");
    run(&passwords, "settle", vec![Value::resource(&root)]);
    assert_eq!(items(&passwords.host).len(), 1);
}
