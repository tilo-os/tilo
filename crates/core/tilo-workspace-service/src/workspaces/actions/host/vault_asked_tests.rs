//! An agent's addition to the vault waits for its person, and what it would keep waits with it
//! in the device's memory alone: the asked record, what the person is shown, the files and the
//! log hold no secret, note or field of it. The person's approve keeps the item with exactly
//! those; a call asked before the device started again cannot run, and its approve says so.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    clippy::large_futures,
    reason = "in a test these are the assertion, and each test keeps its host to the end"
)]

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::asked::{Offer, SECRET_GONE};
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest, person_vault};
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, caller, governed};
use super::turn_use_tests::asks;
use super::vault_tests::{ask, files_holding};
use super::waiting::{answered_by, at_once};
use super::web_call_tests::logged;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{TestGroupKeys, test_host_holding};
use crate::workspaces::tests::governor;

/// A made-up password: no site ever issued it.
const SECRET: &str = "made-up-vault-secret-5521";
/// A made-up note kept with it.
const NOTE: &str = "made-up-recovery-words-8812";
/// A made-up value of a field kept with it.
const PIN: &str = "made-up-pin-6640";
/// The password of the call asked before the device started again.
const LATER: &str = "made-up-later-secret-3307";

/// The vault's add of a login titled `title`, with `secret`, a note and a field.
fn adding(title: &str, secret: &str) -> InvokeRequest {
    let Value::Object(arguments) = json!({
        "kind": "login",
        "belongsTo": ["https://bank.example"],
        "username": "ada@example.test",
        "title": title,
        "notes": NOTE,
        "fields": [{"label": "PIN", "value": PIN}],
        "secret": secret,
    }) else {
        unreachable!("arguments are an object")
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: person_vault::target(person_vault::ADD),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// `request` asked by `agent` on Ada's device: what it answered, or what its person is shown.
async fn asked_by_agent(
    service: &ServiceWorkspaceActions,
    workspace_id: tilo_server_protocol::WorkspaceId,
    (ada, agent): (&DeviceCert, &Caller),
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    let receipt = super::super::tests::settled(tilo_actions::remote::ActionProvider::invoke(
        &service.in_workspace_as(workspace_id, ada),
        agent,
        request.clone(),
    ))
    .await?;
    at_once(
        &service
            .shared
            .workspaces
            .get()
            .and_then(crate::workspaces::WeakWorkspaces::upgrade)
            .unwrap(),
        request,
        receipt,
    )
}

/// Everything the host's asked records show and keep of `workspace`, as text: what the person
/// reads, and the call as its record keeps it.
fn asked_records(workspaces: &Workspaces, workspace: &str) -> String {
    let store = workspaces.asked_store().unwrap();
    let mut text = String::new();
    for waiting in store.waiting_in(workspace) {
        text.push_str(&serde_json::to_string(&waiting.asked).unwrap());
        if let Some(kept) = store.kept_ask(&waiting.asked.record) {
            text.push_str(&serde_json::to_string(&kept).unwrap());
        }
    }
    text
}

/// The calls the workspaces' record of them keeps, as text, once it keeps `invocation`.
async fn kept_on_disk(workspaces: &Workspaces, invocation: InvocationId) -> String {
    let record = invocation.0.to_string();
    tilo_test_env::within("the asked record is written", async {
        loop {
            let kept = workspaces.asked_kept().await;
            if kept.iter().any(|ask| ask.at.record == record) {
                return serde_json::to_string(&kept).unwrap();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}

/// Ada's item titled `title`, its secret, note and fields shown to her one by one.
async fn entry_titled(
    service: &ServiceWorkspaceActions,
    workspace_id: tilo_server_protocol::WorkspaceId,
    ada: &DeviceCert,
    title: &str,
) -> Result<Option<Value>, ActionCallError> {
    let as_ada = caller(ada);
    let listed = ask(
        service,
        workspace_id,
        (ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await?;
    let Some(items) = listed.get("items").and_then(Value::as_array) else {
        unreachable!("listing the vault answers its items");
    };
    let Some(item) = items
        .iter()
        .find(|item| item.get("title").and_then(Value::as_str) == Some(title))
    else {
        return Ok(None);
    };
    let Some(reference) = item.get("reference") else {
        unreachable!("an item listed carries its reference");
    };
    let mut shown = serde_json::Map::new();
    let mut parts = vec![("password".to_owned(), "secret".to_owned())];
    parts.push(("notes".to_owned(), "notes".to_owned()));
    for label in item
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let label = label.as_str().unwrap();
        parts.push((format!("field:{label}"), format!("field:{label}")));
    }
    for (name, part) in parts {
        let value = ask(
            service,
            workspace_id,
            (ada, &as_ada),
            person_vault::REVEAL,
            json!({"item": reference, "part": part}),
        )
        .await?;
        let Some(value) = value.get("value") else {
            unreachable!("a revealed part carries its value");
        };
        shown.insert(name, value.clone());
    }
    let fields: serde_json::Map<String, Value> = shown
        .iter()
        .filter_map(|(name, value)| Some((name.strip_prefix("field:")?.to_owned(), value.clone())))
        .collect();
    let Some(password) = shown.get("password") else {
        unreachable!("every item is shown its password");
    };
    let Some(notes) = shown.get("notes") else {
        unreachable!("every item is shown its notes");
    };
    Ok(Some(json!({
        "password": password,
        "notes": notes,
        "fields": fields,
    })))
}

#[tokio::test]
async fn an_asked_vault_addition_keeps_its_secret_in_memory_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let keys = Arc::new(TestGroupKeys::default());
    let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let agent = caller(&ada).delegated_to("assistant");
    let (lines, logging) = logged();
    tracing::info!("the line that shows the log is read");

    // The agent's addition waits for Ada; nothing she is shown, and nothing its record keeps,
    // holds what it would keep.
    let bank = adding("Bank", SECRET);
    let facts = asks(asked_by_agent(&service, workspace_id, (&ada, &agent), &bank).await);
    let shown = serde_json::to_string(&facts).unwrap() + &asked_records(workspaces, &workspace);
    let on_disk = kept_on_disk(workspaces, bank.invocation).await;
    assert!(on_disk.contains("\"sealed\""), "{on_disk}");
    for secret in [SECRET, NOTE, PIN] {
        assert!(!shown.contains(secret), "{secret} shown: {shown}");
        assert!(!on_disk.contains(secret), "{secret} kept: {on_disk}");
    }

    // Approved, the item is kept with exactly what the agent asked to keep.
    answered_by(
        &service.in_workspace_as(workspace_id, &ada),
        &caller(&ada),
        (&agent, &bank),
        Offer::Approve,
    )
    .await
    .expect("approved, the item is kept");
    let entry = entry_titled(&service, workspace_id, &ada, "Bank")
        .await
        .expect("the vault lists for Ada")
        .expect("the approved item is kept");
    assert_eq!(entry["password"], SECRET, "{entry}");
    assert_eq!(entry["notes"], NOTE);
    assert_eq!(entry["fields"], json!({"PIN": PIN}));

    // A second addition waits as the device starts again: its secret went with the device's
    // memory, so its approve is refused saying so, and nothing is kept.
    let later = adding("Later", LATER);
    asks(asked_by_agent(&service, workspace_id, (&ada, &agent), &later).await);
    let on_disk = kept_on_disk(workspaces, later.invocation).await;
    assert!(!on_disk.contains(LATER), "{on_disk}");
    server.shutdown().await;

    let server = test_host_holding(dir.path(), &key, &owner, keys).await;
    let workspaces = server.workspaces();
    let service = workspaces.inner.jobs.governance().unwrap();
    let waiting = workspaces.asked_store().unwrap().waiting_in(&workspace);
    assert_eq!(
        waiting.len(),
        1,
        "the call still waits for Ada: {waiting:?}"
    );
    let refused = answered_by(
        &service.in_workspace_as(workspace_id, &ada),
        &caller(&ada),
        (&agent, &later),
        Offer::Approve,
    )
    .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains(SECRET_GONE)),
        "{refused:?}"
    );
    assert!(
        entry_titled(&service, workspace_id, &ada, "Later")
            .await
            .expect("the vault lists for Ada")
            .is_none(),
        "nothing is kept without its secret"
    );
    assert!(
        workspaces
            .asked_store()
            .unwrap()
            .waiting_in(&workspace)
            .is_empty(),
        "it waits no more"
    );

    drop(logging);
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(logged.contains("the line that shows the log is read"));
    for secret in [SECRET, NOTE, PIN, LATER] {
        assert!(!logged.contains(secret), "{secret} logged");
        let clear = files_holding(dir.path(), secret.as_bytes());
        assert!(clear.is_empty(), "{secret} in the clear: {clear:?}");
    }
    server.shutdown().await;
}

/// A made-up mail password: no server ever issued it.
const MAIL_PASSWORD: &str = "made-up-mail-password-7718";
/// The mail password of the call asked before the device started again.
const LATER_MAIL_PASSWORD: &str = "made-up-later-mail-password-2290";

/// The mail account `address` added with `password` through the workspace's own governance
/// action (`workspace/mail-accounts#add`), whose published `password` is secret.
fn account_adding(workspace: &str, address: &str, password: &str) -> InvokeRequest {
    use tilo_actions::remote::{
        WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_MAIL_ACCOUNTS, workspace_governance_child,
    };
    let Value::Object(arguments) = json!({
        "address": address,
        "displayName": "Ada Lovelace",
        "incomingHost": "imap.lab.example.test",
        "incomingPort": 993,
        "outgoingHost": "smtp.lab.example.test",
        "outgoingPort": 465,
        "password": password,
    }) else {
        unreachable!("arguments are an object")
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: WORKSPACE_GOVERNANCE_PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(
                workspace_governance_child(workspace, Some(WORKSPACE_MAIL_ACCOUNTS)),
                "add",
            ),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The addresses of the mail accounts the workspace keeps, as Ada reads them.
async fn kept_accounts(
    service: &ServiceWorkspaceActions,
    workspace_id: tilo_server_protocol::WorkspaceId,
    ada: &DeviceCert,
) -> Result<Vec<String>, ActionCallError> {
    use tilo_actions::remote::ActionProvider as _;
    use tilo_actions::remote::person_accounts;

    let page = service.in_workspace_as(workspace_id, ada);
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: person_accounts::target(person_accounts::LIST),
        argument: InvokeArgument::Json(json!({}).as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt = super::super::tests::settled(page.invoke(&caller(ada), request)).await?;
    let tilo_actions::remote::ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the host answers at once, not {:?}", receipt.status);
    };
    let listed = output.value.unwrap_or_default();
    let Some(accounts) = listed.get("accounts").and_then(Value::as_array) else {
        unreachable!("listing the accounts answers them");
    };
    Ok(accounts
        .iter()
        .filter_map(|account| {
            account
                .get("address")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}

/// What an action publishes as secret waits in the device's memory alone, whatever the action:
/// a migration agent's mail account waits for its person with its password in nothing she is
/// shown and nothing its record keeps; her approve signs it in with that password; one asked
/// before the device started again cannot run, and its approve says so.
#[tokio::test]
async fn an_asked_mail_account_keeps_its_password_in_memory_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let keys = Arc::new(TestGroupKeys::default());
    let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let workspaces = server.workspaces();
    workspaces.sign_in_accounts_through(|_, password| {
        if password == MAIL_PASSWORD {
            Ok("signed in".into())
        } else {
            Err("IMAP server rejected the credential".into())
        }
    });
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let agent = caller(&ada).delegated_to(tilo_actions::remote::device_agent_id(
        &ada.user_public_key,
        "migrate",
    ));

    // The agent's account waits for Ada; nothing she is shown, and nothing its record keeps,
    // holds its password.
    let first = account_adding(&workspace, "ada@lab.example.test", MAIL_PASSWORD);
    let facts = asks(asked_by_agent(&service, workspace_id, (&ada, &agent), &first).await);
    let shown = serde_json::to_string(&facts).unwrap() + &asked_records(workspaces, &workspace);
    let on_disk = kept_on_disk(workspaces, first.invocation).await;
    assert!(on_disk.contains("\"sealed\":[\"password\"]"), "{on_disk}");
    assert!(on_disk.contains("ada@lab.example.test"), "{on_disk}");
    assert!(!shown.contains(MAIL_PASSWORD), "shown: {shown}");
    assert!(!on_disk.contains(MAIL_PASSWORD), "kept: {on_disk}");

    // Approved, it signs in with the password the agent sent, which only that one does.
    let added = answered_by(
        &service.in_workspace_as(workspace_id, &ada),
        &caller(&ada),
        (&agent, &first),
        Offer::Approve,
    )
    .await
    .expect("approved, the account signs in and is kept");
    assert_eq!(added["address"], "ada@lab.example.test", "{added}");

    // A second account waits as the device starts again: its password went with the device's
    // memory, so its approve is refused saying so, and nothing is kept.
    let later = account_adding(&workspace, "bos@lab.example.test", LATER_MAIL_PASSWORD);
    asks(asked_by_agent(&service, workspace_id, (&ada, &agent), &later).await);
    let on_disk = kept_on_disk(workspaces, later.invocation).await;
    assert!(!on_disk.contains(LATER_MAIL_PASSWORD), "{on_disk}");
    server.shutdown().await;

    let server = test_host_holding(dir.path(), &key, &owner, keys).await;
    let workspaces = server.workspaces();
    let service = workspaces.inner.jobs.governance().unwrap();
    let refused = answered_by(
        &service.in_workspace_as(workspace_id, &ada),
        &caller(&ada),
        (&agent, &later),
        Offer::Approve,
    )
    .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains(SECRET_GONE)),
        "{refused:?}"
    );
    let kept = kept_accounts(&service, workspace_id, &ada)
        .await
        .expect("Ada reads the accounts");
    assert!(
        kept.contains(&"ada@lab.example.test".to_owned()),
        "{kept:?}"
    );
    assert!(
        !kept.contains(&"bos@lab.example.test".to_owned()),
        "{kept:?}"
    );
    let clear = files_holding(dir.path(), LATER_MAIL_PASSWORD.as_bytes());
    assert!(clear.is_empty(), "the password in the clear: {clear:?}");
    server.shutdown().await;
}

/// What a device takes for secret is what an action is published with where the device holds
/// its descriptor: the vault's add names its secret, note and fields, a governance verb its
/// own. An action a provider registered here lists only as it is asked, a secret among its
/// parameters, is described nowhere the device reads at once, so every argument of a call of
/// it is taken for a secret.
#[tokio::test]
async fn an_action_listed_only_as_it_is_asked_is_taken_for_all_secret() {
    use tilo_actions::remote::{CataloguePage, RemoteActionTarget, TestProvider};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host_holding(dir.path(), &key, &owner, Arc::default()).await;
    let workspaces = server.workspaces();
    let mut row = person_vault::call_rows().remove(0);
    row.provider = "test.dynamic".to_owned();
    let mut action = row.action.take().unwrap();
    action.address = tilo_actions::ActionAddress::new("dynamic.accounts", "connect");
    action.params.push(tilo_actions::ActionParam::required(
        "token",
        tilo_actions::ParamType::Secret,
        "The token it connects with.",
    ));
    let target = RemoteActionTarget {
        provider: row.provider.clone(),
        address: action.address.clone(),
    };
    row.action = Some(action);
    let listed = Arc::new(TestProvider::new().with_catalogue(move |_, query| {
        Ok(CataloguePage::from_rows(
            "1".into(),
            vec![row.clone()],
            &query,
        ))
    }));
    workspaces.providers().provide(
        "test.dynamic",
        "test.dynamic",
        crate::workspaces::providers::Reach::Devices,
        listed,
    );
    let secrets = workspaces.secret_parameters();
    assert_eq!(secrets(&target), None, "described nowhere read at once");
    let mut vault_add = secrets(&person_vault::target(person_vault::ADD)).unwrap();
    vault_add.sort();
    assert_eq!(vault_add, ["fields", "notes", "secret"]);
    assert_eq!(
        secrets(&RemoteActionTarget {
            provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.into(),
            address: tilo_actions::ActionAddress::new("workspace.permissions:w", "rename"),
        }),
        Some(Vec::new()),
        "a governance verb is described, and takes no secret"
    );
    server.shutdown().await;
}
