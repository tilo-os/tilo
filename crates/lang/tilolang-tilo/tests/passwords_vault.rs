//! Passwords keeps every secret in the workspace's vault: its records name an entry there and
//! hold no secret. A workspace an older Passwords kept plaintext in has each secret carried into
//! the vault once, and the record emptied, the first time the vault settles.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, Value};
use tilolang_tilo::host::Host;

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

/// What the app asks of the vault, by verb and its arguments by name.
#[derive(Default)]
struct Asked(Rc<RefCell<Vec<(String, Vec<(String, Value)>)>>>);

impl Invoker for Asked {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        match invocation {
            Invocation::Action {
                app, name, args, ..
            } if app.as_str() == "tilo.vault" => {
                self.0.borrow_mut().push((name.clone(), args.clone()));
                Invoked::Done(Value::Unit)
            }
            other => Invoked::Refused(format!("this test answers only the vault, not {other:?}")),
        }
    }
}

impl Asked {
    /// The calls of `verb` so far.
    fn of(&self, verb: &str) -> Vec<Vec<(String, Value)>> {
        self.0
            .borrow()
            .iter()
            .filter(|(name, _)| name == verb)
            .map(|(_, args)| args.clone())
            .collect()
    }
}

/// The argument `name` of a call, as text.
fn argued(args: &[(String, Value)], name: &str) -> String {
    match args.iter().find(|(arg, _)| arg == name) {
        Some((_, Value::Text(text))) => text.clone(),
        Some((_, Value::Opt(Some(inner)))) => match &**inner {
            Value::Text(text) => text.clone(),
            other => panic!("{name} is a text argument: {other:?}"),
        },
        other => panic!("{name} is a text argument: {other:?}"),
    }
}

/// The vault as the daemon publishes it to Passwords' window.
fn vault() -> ResourceRef {
    ResourceRef::parse_id("vault", "tilo.vault#Vault").expect("the vault's id")
}

/// The vault reading as the daemon answers it, holding `entries`.
fn read_vault(host: &Host, entries: serde_json::Value) {
    host.runtime()
        .set_reading(&vault(), &json!({ "entries": entries }).to_string())
        .expect("the vault's reading");
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

fn run(replica: &Replica, name: &str, args: Vec<Value>) -> Value {
    let outcome = replica
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (action_id(&replica.host, name).as_str(), args),
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
        "title", "entry", "username", "password", "notes", "origins", "fields", "tags",
    ]
    .iter()
    .map(|name| format!("{:?}", field(host, item, name)))
    .collect::<Vec<_>>()
    .join(" ")
}

/// A Passwords over `doc`, asking the vault through a recorder.
fn passwords(doc: Workspace) -> (Replica, Asked) {
    let (replica, _) = Replica::new("ada", doc, &[PASSWORDS]);
    let asked = Asked::default();
    replica
        .host
        .runtime()
        .set_foreign_invoker(Asked(Rc::clone(&asked.0)));
    read_vault(&replica.host, json!([]));
    (replica, asked)
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
    let (today, asked) = passwords(before.doc.fork());
    let root = today.host.root_of(APP).expect("the root");
    let [bank] = &items(&today.host)[..] else {
        panic!("the older record is there");
    };
    assert!(record_words(&today.host, &bank).contains(PASSWORD));
    run(&today, "settle", vec![Value::resource(&root)]);

    let carried = asked.of("carry");
    let [carry] = &carried[..] else {
        panic!("one entry is carried: {carried:?}");
    };
    assert_eq!(argued(carry, "password"), PASSWORD);
    assert_eq!(argued(carry, "username"), "ada");
    assert_eq!(argued(carry, "title"), "Bank");
    let entry = argued(carry, "id");
    assert_eq!(text(&today.host, &bank, "entry"), entry);
    assert!(
        entry.len() == 32 && entry.chars().all(|c| c.is_ascii_hexdigit()),
        "an entry id of the vault: {entry}"
    );
    let left = record_words(&today.host, &bank);
    assert!(
        !left.contains(PASSWORD),
        "the record holds no secret: {left}"
    );
    assert!(
        !left.contains("bank.example"),
        "nor where it signs in: {left}"
    );
    assert_eq!(text(&today.host, &bank, "username"), "");

    // The vault answers with the entry now: settling again carries nothing more.
    read_vault(
        &today.host,
        json!([{
            "id": entry, "kind": "login", "title": "Bank", "username": "ada",
            "password": PASSWORD, "notes": "", "origins": ["https://bank.example"],
            "tags": [], "fields": [], "created": 1, "updated": 1,
            "changed": "1970-01-01T00:00:01Z", "stale": true,
        }]),
    );
    run(&today, "settle", vec![Value::resource(&root)]);
    assert_eq!(asked.of("carry").len(), 1, "an entry is carried once");
    assert_eq!(items(&today.host).len(), 1, "and listed once");
}

#[test]
fn a_new_item_is_an_entry_of_the_vault_and_its_record_holds_no_secret() {
    let (passwords, asked) = passwords(Workspace::new());
    let root = passwords.host.root_of(APP).expect("the root");
    run(
        &passwords,
        "new_item",
        vec![Value::resource(&root), Value::text("Mail")],
    );
    let kept = asked.of("keep");
    let [keep] = &kept[..] else {
        panic!("the item is kept in the vault: {kept:?}");
    };
    assert_eq!(argued(keep, "title"), "Mail");
    let [mail] = &items(&passwords.host)[..] else {
        panic!("one item");
    };
    assert_eq!(text(&passwords.host, &mail, "entry"), argued(keep, "id"));

    // An entry kept elsewhere (a mail account's password, another device's login) gets an item,
    // and the item holds none of what the entry keeps.
    read_vault(
        &passwords.host,
        json!([
            {
                "id": argued(keep, "id"), "kind": "login", "title": "Mail", "username": "",
                "password": "", "notes": "", "origins": [], "tags": [], "fields": [],
                "created": 1, "updated": 1, "changed": "1970-01-01T00:00:01Z", "stale": false,
            },
            {
                "id": "fedcba9876543210fedcba9876543210", "kind": "api_key", "title": "Model key",
                "username": "ada", "password": "sk-live-key", "notes": "", "origins": [],
                "tags": ["work"], "fields": [], "created": 1, "updated": 1,
                "changed": "1970-01-01T00:00:01Z", "stale": false,
            }
        ]),
    );
    run(&passwords, "settle", vec![Value::resource(&root)]);
    let listed = items(&passwords.host);
    assert_eq!(listed.len(), 2, "the entry kept elsewhere is listed");
    let key = listed
        .iter()
        .find(|item| text(&passwords.host, item, "title") == "Model key")
        .expect("the key's item");
    let words = record_words(&passwords.host, key);
    assert!(!words.contains("sk-live-key"), "{words}");
    assert!(words.contains("work"), "its tags are searched: {words}");
    assert!(asked.of("carry").is_empty(), "nothing old to carry");
}
