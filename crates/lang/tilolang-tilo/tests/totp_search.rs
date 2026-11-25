//! The Authenticator's accounts in the system's search: an account is what the app publishes of
//! it, the device's search finds it by its issuer, and choosing the hit shows that account's own
//! page — the account the hit named, not the app's list and not whichever account stood first.
//!
//! The account's key is the vault's: what the app publishes holds the issuer and the label and
//! nothing of the seed, so the index holds no key and no code, as a record does not.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::model::{OPEN_ANCHOR, Source as _, Target};
use tilo_resource::search_index::{IndexEntry, IndexState, Publisher};
use tilo_workspace_service::search::{AppContentIndex, IndexSearch};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::host_work::{EngineHost, JobHandlers, VaultCode, VaultHost};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{UiNode, Value};
use tilolang_tilo::publish::resource_id;

use crate::fixtures::{Replica, Workspace, call, target};

const APP: &str = "so.tilo.totp";
const SOURCE: &str = include_str!("../../../../apps/tl/totp/app.tl");
const IDS: &str = include_str!("../../../../apps/tl/totp/app.ids");
const VAULT: &str = include_str!("../../../../apps/tl/vault/surface.toml");

/// The two accounts' seeds, as the services gave them.
const ACME_KEY: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
const GITHUB_KEY: &str = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";
/// The one code this device's vault makes, for any item.
const CODE: &str = "123456";

/// The device's engine: every key reads, as the engine's check answers.
struct Engine;

impl EngineHost for Engine {
    fn call(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
        match op {
            "check" => Ok(r#"{"ok": true}"#.to_owned()),
            other => Err(format!("this device has no `{other}`")),
        }
    }

    fn start(
        &self,
        _app: &str,
        _op: &str,
        _input: &str,
        _handlers: &JobHandlers,
    ) -> Result<String, String> {
        Err("this device starts no job".to_owned())
    }

    fn job(&self, _path: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(None)
    }

    fn cancel(&self, _job: &str) -> bool {
        false
    }
}

/// The workspace's vault, as the daemon serves it: it keeps each item's seed by its reference and
/// answers the code of this moment, as no window and no record sees the seed.
#[derive(Clone, Default)]
struct Vault {
    kept: Rc<RefCell<BTreeMap<String, String>>>,
    next: Rc<RefCell<u32>>,
}

impl Vault {
    /// The seeds it keeps, by item.
    fn kept(&self) -> BTreeMap<String, String> {
        self.kept.borrow().clone()
    }
}

/// The argument `name` of a vault call, as text.
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

impl Invoker for Vault {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let Invocation::Action {
            app, name, args, ..
        } = invocation
        else {
            return Invoked::Failed("the vault answers actions alone".to_owned());
        };
        if app.as_str() != "so.tilo.vault" {
            return Invoked::Failed(format!("this device's vault is no {app}"));
        }
        match name.as_str() {
            "keep" => {
                let reference = {
                    let mut next = self.next.borrow_mut();
                    *next = next.saturating_add(1);
                    format!("vault.item:{:032x}", *next)
                };
                self.kept
                    .borrow_mut()
                    .insert(reference.clone(), argued(args, "secret"));
                Invoked::Done(Value::text(reference))
            }
            "forget" => {
                self.kept.borrow_mut().remove(&argued(args, "item"));
                Invoked::Done(Value::Unit)
            }
            other => Invoked::Failed(format!("the vault has no `{other}`")),
        }
    }
}

impl VaultHost for Vault {
    fn code(&self, reference: &str) -> Option<VaultCode> {
        if !self.kept.borrow().contains_key(reference) {
            return None;
        }
        Some(VaultCode {
            code: CODE.to_owned(),
            until: "2030-01-01T00:00:30Z".to_owned(),
            period: 30,
            digits: 6,
            algorithm: "SHA1".to_owned(),
        })
    }
}

/// A person's device running the Authenticator, its vault and engine beside it, over an empty
/// workspace.
fn seat() -> (Replica, Vault) {
    let (replica, _) = Replica::new("ada", Workspace::new(), &[]);
    let vault = Vault::default();
    replica.host.runtime().set_engine(Engine);
    replica.host.runtime().set_vault(vault.clone());
    replica.host.runtime().set_foreign_invoker(vault.clone());
    // The Authenticator's keys are the vault package's to keep, its surface published beside the
    // app: the app compiles against it and calls it as every window does.
    let mut metas = tilolang_tilo::platform_apps::named_platform_packages(SOURCE)
        .expect("the device's surface reads");
    metas.push(
        tilolang_tilo::catalogue::described("so.tilo.vault", "Vault", VAULT)
            .expect("the vault's surface reads"),
    );
    for meta in &metas {
        replica
            .host
            .runtime()
            .load_foreign(meta.clone())
            .expect("a package the app names is described here");
    }
    let compiled = tilolang::compile_package(
        SOURCE,
        &tilolang::IdsLock::parse(IDS).expect("the app's ids lock"),
        &metas,
    )
    .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    replica
        .host
        .load_compiled(&compiled)
        .expect("the app loads");
    (replica, vault)
}

/// Every text a view draws, by its nodes.
fn texts(node: &UiNode, out: &mut Vec<String>) {
    for value in node.props.values() {
        if let Value::Text(text) = value {
            out.push(text.clone());
        }
    }
    for child in &node.children {
        texts(child, out);
    }
}

/// Add an account as the app's own action does, answering the account it made.
fn add(replica: &Replica, root: &ResourceRef, issuer: &str, label: &str, key: &str) -> ResourceRef {
    target(&call(
        replica,
        "ada",
        APP,
        "add_account",
        vec![
            Value::resource(root),
            Value::text(issuer),
            Value::text(label),
            Value::text(key),
            Value::text("SHA1"),
            Value::Int(6),
            Value::Int(30),
            Value::Int(0),
        ],
    ))
}

/// Searching the device's index for `needle`, as the shell searches it for the person.
fn search(entries: Vec<IndexEntry>, needle: &str) -> Vec<tilo_desktop_search::Hit> {
    let index = AppContentIndex::in_memory();
    let published = tilolang_tilo::search::publish_whole(
        Publisher::default(),
        IndexState {
            generation: 1,
            secrets_unlocked: None,
        },
        entries,
    );
    for delta in &published.deltas {
        index
            .publish("workspace", APP, "vault", delta)
            .expect("the index takes what the app publishes");
    }
    let search = IndexSearch::new(Arc::new(index), Arc::new(|_, _| Box::pin(async { true })));
    IndexedContent::new(
        Arc::new(search),
        tilo_actions::remote::Caller::local_person("ada"),
        str::to_owned,
        "ada",
        Arc::new(|| {}),
    )
    .query(needle)
}

/// Two accounts, found by the second one's issuer: the hit names that account, opening it shows
/// that account's page, and the index holds neither account's key nor a code.
#[test]
fn a_hit_on_an_account_opens_that_account_and_holds_no_key() {
    let (replica, vault) = seat();
    let root = replica.root(APP);
    let acme = add(&replica, &root, "Acme", "bob", ACME_KEY);
    let github = add(&replica, &root, "GitHub", "ada@example.com", GITHUB_KEY);
    assert_eq!(
        vault.kept().len(),
        2,
        "the vault keeps both seeds, and the records keep none"
    );
    for field in ["secret", "key", "code"] {
        assert!(
            replica.host.runtime().read(&github, field).is_err(),
            "the account has no {field}"
        );
    }

    // What the app publishes of its accounts: the index holds no key and no code, only the words
    // a person reads — the issuer, the label and the vault's reference.
    let entries = tilolang_tilo::search::whole(&replica.host);
    assert!(!entries.is_empty(), "the app publishes its accounts");
    let held = format!("{entries:?}");
    assert!(
        !held.contains(ACME_KEY) && !held.contains(GITHUB_KEY),
        "no seed reaches the index: {held}"
    );
    assert!(!held.contains(CODE), "no code reaches the index: {held}");

    let anchor = chosen_account(entries, &root, &github);
    shows_the_account_it_names(&replica, &anchor, &github, &acme);
}

/// The system's search, by the second account's issuer: the anchor of the account its hit names.
fn chosen_account(entries: Vec<IndexEntry>, root: &ResourceRef, github: &ResourceRef) -> String {
    let hits = search(entries, "GitHub");
    let hit = hits
        .iter()
        .find(|hit| hit.title == "GitHub")
        .unwrap_or_else(|| panic!("the account is found by its issuer: {hits:#?}"));
    assert!(
        !hits.iter().any(|hit| hit.title == "Acme"),
        "the other account holds none of the words: {hits:#?}"
    );
    let Target::Invoke(chosen) = &hit.target else {
        panic!("the hit opens the app: {:?}", hit.target);
    };
    assert_eq!(
        chosen.address.resource,
        resource_id(APP, &root.id_string()),
        "the account is an item of the app's record, which its open is on"
    );
    assert_eq!(chosen.address.action, "open");
    let anchor = chosen
        .arguments
        .get(OPEN_ANCHOR)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("the hit names the account: {chosen:?}"))
        .to_owned();
    assert_eq!(
        anchor,
        resource_id(APP, &github.id_string()),
        "the hit is the GitHub account, not the one added first"
    );

    anchor
}

/// Opening what the hit names: the window's own decision opens the account's default view,
/// and the page draws that account, not the app's list and not the other account.
fn shows_the_account_it_names(
    replica: &Replica,
    anchor: &str,
    github: &ResourceRef,
    acme: &ResourceRef,
) {
    let plain = anchor
        .strip_prefix("tl:")
        .and_then(|rest| rest.split_once(':'))
        .map_or(anchor, |(_, id)| id);
    let plan = replica
        .host
        .open_plan(APP, plain, None)
        .expect("the hit names a resource of the workspace");
    assert_eq!(
        plan.target.as_ref().map(ResourceRef::id_string),
        Some(github.id_string()),
        "the deepest page between the root and the account is the account's"
    );
    assert_ne!(
        plan.target.as_ref().map(ResourceRef::id_string),
        Some(acme.id_string()),
        "the hit opens the account it named, not the one added first"
    );
    let page = replica
        .host
        .open_resource(
            plan.target.as_ref().expect("the account opens"),
            plan.view.as_deref(),
        )
        .expect("the account's page opens");
    let mut drawn = Vec::new();
    texts(page.tree(), &mut drawn);
    assert!(
        drawn.iter().any(|word| word == "GitHub"),
        "the account's issuer shows: {drawn:?}"
    );
    assert!(
        drawn.iter().any(|word| word == "ada@example.com"),
        "the account's label shows: {drawn:?}"
    );
    assert!(
        !drawn.iter().any(|word| word == "Acme"),
        "the account added first is not shown: {drawn:?}"
    );
    assert!(
        !drawn.iter().any(|word| word == "Add account"),
        "the app's list, not the account's page, would offer Add account: {drawn:?}"
    );
}
