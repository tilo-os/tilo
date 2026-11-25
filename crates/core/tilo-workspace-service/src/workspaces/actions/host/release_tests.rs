//! A vault item released to an app's held connection ([`crate::workspaces::vault_release`]):
//! the device holds the bridge's connection and keeps two made-up tokens in the workspace's
//! vault. The first release asks the person, once; their "Approve and grant" lets the bridge read
//! that one item, and the next release answers exactly the token until they take that leave
//! back. Every other caller is refused saying
//! what is missing, and the token is in no record, no argument, no log line, no use and no file
//! in the clear.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    clippy::large_futures,
    reason = "in a test these are the assertion, and each test keeps its host to the end"
)]

use std::sync::Arc;

use accounts::vault_items;
use serde_json::{Value, json};
use tilo_actions::remote::asked::{Offer, RecordRef, Resolve};
use tilo_actions::remote::person_vault::{self, ItemRef, Released};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    workspace_invocations,
};
use tilo_actions::{ActionAddress, InvocationId};

use super::super::tests::{caller, governed};
use super::turn_use_tests::on_turn;
use super::vault_tests::{ask, ask_at, files_holding};
use super::web_call_tests::logged;
use crate::work::{HostConnections, Order, Turn, takes_turn_in};
use crate::workspaces::Workspaces;
use crate::workspaces::tests::{governor, test_host};
use crate::workspaces::vault_release::{self as release, AppProcess};

/// The app whose connection is released a token.
const BRIDGE: &str = "so.tilo.bridge";
/// A made-up bot token: no service ever issued it.
const TOKEN: &str = "made-up-bot-token-0123456789abcdef";
/// Another made-up token, in an item the bridge is never allowed.
const OTHER: &str = "made-up-other-token-fedcba9876543210";
/// Where the bridge's token belongs.
const GATEWAY: &str = "https://gateway.example.test";

/// The bridge, installed here with what it needs; its process is the daemon's, so none runs.
struct Bridge;

impl HostConnections for Bridge {
    fn connections(&self) -> Vec<(String, Vec<String>)> {
        vec![(BRIDGE.to_owned(), Vec::new())]
    }
    fn installed(&self, app: &str) -> bool {
        app == BRIDGE
    }
    fn gives(&self, _: &str, _: &str, _: &str) -> bool {
        true
    }
    fn on_power(&self) -> bool {
        true
    }
    fn start(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn runs(&self, _: &str, _: &str) -> bool {
        false
    }
    fn stop(&self, _: &str, _: &str) {}
}

/// The release of `item`, as the bridge's process calls it: no arguments.
fn release_of(item: &ItemRef) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: person_vault::release_target(item),
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The person's "Approve and grant" of their asked record `record`.
pub(super) fn approve_and_grant(record: &RecordRef) -> InvokeRequest {
    let resolving = Resolve {
        resolution: Offer::ApproveAndGrant,
        ..Resolve::default()
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(&record.record, tilo_actions::remote::asked::RESOLVE),
        },
        argument: InvokeArgument::Json(
            serde_json::to_value(resolving)
                .unwrap()
                .as_object()
                .cloned()
                .unwrap(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The reason `taken` was refused with.
pub(super) fn refusal<T: std::fmt::Debug>(taken: Result<T, ActionCallError>) -> String {
    match taken {
        Err(ActionCallError::Refused { reason }) => reason,
        other => panic!("refused: {other:?}"),
    }
}

/// A key the person keeps in the vault, belonging to `place`: its reference.
async fn kept(
    service: &super::super::ServiceWorkspaceActions,
    workspace_id: tilo_server_protocol::WorkspaceId,
    owner: &tilo_user_identity::DeviceCert,
    (place, secret): (&str, &str),
) -> ItemRef {
    let added = ask(
        service,
        workspace_id,
        (owner, &caller(owner)),
        person_vault::ADD,
        json!({"kind": "key", "belongsTo": [place], "secret": secret}),
    )
    .await
    .expect("the person keeps a key");
    ItemRef::parse(added["reference"].as_str().unwrap()).unwrap()
}

/// Everything the workspace's asked records keep, as text.
pub(super) fn asked_records(workspaces: &Workspaces, workspace: &str) -> String {
    let store = workspaces.asked_store().unwrap();
    let mut text = String::new();
    for waiting in store.waiting_in(workspace) {
        text.push_str(&serde_json::to_string(&waiting.asked).unwrap());
        if let Some(kept) = store.kept_ask(&waiting.asked.record) {
            text.push_str(&serde_json::to_string(&kept.request).unwrap());
            text.push_str(&serde_json::to_string(&kept.facts).unwrap());
        }
    }
    text
}

/// The order here: this is the only device that runs the bridge, so it waits for no other.
const AT_ONCE: Order = Order {
    server_grace: std::time::Duration::ZERO,
    other_grace: std::time::Duration::ZERO,
};

/// The bridge's held connection asks for its token: the person is asked once, their "Approve
/// and grant" covers that one item, and the release answers exactly the token. A second item
/// stays asked for. Nothing writes the token down.
#[tokio::test]
async fn a_held_connection_is_released_the_one_item_its_person_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let token = kept(&service, workspace_id, &owner, (GATEWAY, TOKEN)).await;
    let other = kept(
        &service,
        workspace_id,
        &owner,
        ("https://other.example.test", OTHER),
    )
    .await;

    workspaces.run_connections(Arc::new(Bridge));
    takes_turn_in(
        workspaces,
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        AT_ONCE,
    )
    .await
    .expect("this device takes the bridge's connection");
    let connection = AppProcess::connection(BRIDGE);

    let (lines, guard) = logged();
    tracing::info!("the line that shows the log is read");

    // Asked once: the second call finds their question still waiting and writes no other.
    let asked = refusal(
        workspaces
            .release_to((BRIDGE, &workspace), &connection, &release_of(&token))
            .await,
    );
    assert_eq!(asked, release::asked_first(BRIDGE));
    let again = refusal(
        workspaces
            .release_to((BRIDGE, &workspace), &connection, &release_of(&token))
            .await,
    );
    assert_eq!(again, asked);
    let store = workspaces.asked_store().unwrap();
    let waiting = store.waiting_in(&workspace);
    assert_eq!(waiting.len(), 1, "one question for the one item");
    let question = &waiting[0].asked;
    assert_eq!(
        question.offers,
        vec![Offer::Approve, Offer::ApproveAndGrant, Offer::Decline],
        "the person may allow it always"
    );
    assert!(
        question
            .facts
            .facts
            .iter()
            .any(|fact| fact.value.contains(GATEWAY)),
        "the person sees where the item goes: {:?}",
        question.facts
    );
    let shown = asked_records(workspaces, &workspace);
    assert!(
        !shown.contains(TOKEN),
        "the question holds no token: {shown}"
    );

    // Their "Approve and grant" lets the bridge read this one item, and nothing else.
    let person = caller(&owner).in_workspace(&workspace);
    let answered = super::super::tests::settled(
        workspaces.resolve_asked(&person, &approve_and_grant(&question.record)),
    )
    .await
    .expect("their answer is taken");
    assert!(
        matches!(answered.status, ReceiptStatus::Completed { .. }),
        "{answered:?}"
    );
    // The held connection is released exactly the token.
    let call = release_of(&token);
    let released: Released = workspaces
        .release_to((BRIDGE, &workspace), &connection, &call)
        .await
        .expect("the held connection reads the item it was allowed");
    assert_eq!(released.secret, TOKEN);
    assert!(
        !format!("{released:?}").contains(TOKEN),
        "printing the answer prints no token"
    );

    // The grant names one item: the other one is asked for anew, and is not released.
    let not_covered = refusal(
        workspaces
            .release_to((BRIDGE, &workspace), &connection, &release_of(&other))
            .await,
    );
    assert_eq!(not_covered, release::asked_first(BRIDGE));
    assert_eq!(
        store.waiting_in(&workspace).len(),
        1,
        "the other item waits"
    );

    // Their person reads the bridge's leave beside the item's uses; taking back an agent's
    // leaves leaves it, taking back the bridge's ends it, and the next release asks again.
    let mine = (&owner, &caller(&owner));
    let leaves = |answer: Value| answer["always"].as_array().cloned().unwrap_or_default();
    let read = ask(
        &service,
        workspace_id,
        mine,
        person_vault::USES,
        json!({"item": token.to_string()}),
    )
    .await
    .expect("the person reads who may use the item");
    let [leave] = &leaves(read)[..] else {
        panic!("the bridge's leave is listed");
    };
    assert_eq!(leave["app"], BRIDGE);
    assert_eq!(leave["using"], person_vault::RELEASE);
    assert!(leave.get("agent").is_none(), "{leave}");
    assert!(leave["since"].as_u64().unwrap() > 0, "{leave}");
    let others = ask(
        &service,
        workspace_id,
        mine,
        person_vault::USES,
        json!({"item": other.to_string()}),
    )
    .await
    .unwrap();
    assert!(leaves(others).is_empty(), "the leave covers one item");
    let kept_on = ask(
        &service,
        workspace_id,
        mine,
        person_vault::TAKE_BACK,
        json!({"item": token.to_string(), "agent": "scout"}),
    )
    .await
    .unwrap();
    assert_eq!(
        leaves(kept_on).len(),
        1,
        "an agent's take-back leaves the app's"
    );
    workspaces
        .release_to((BRIDGE, &workspace), &connection, &call)
        .await
        .expect("still allowed");
    let taken = ask(
        &service,
        workspace_id,
        mine,
        person_vault::TAKE_BACK,
        json!({"item": token.to_string(), "app": BRIDGE}),
    )
    .await
    .expect("the person takes the bridge's leave back");
    assert!(leaves(taken).is_empty(), "the leave is gone");
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &connection, &call)
                .await
        ),
        release::asked_first(BRIDGE),
        "once taken back, the bridge is asked for again"
    );

    // The token is in no record, no argument, no use written into the item, no log line and
    // no file in the clear.
    drop(guard);
    let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("the line that shows the log is read"));
    assert!(
        !logged.contains(TOKEN),
        "a log line holds the token: {logged}"
    );
    assert!(!asked_records(workspaces, &workspace).contains(TOKEN));
    assert!(!serde_json::to_string(&call).unwrap().contains(TOKEN));
    let listed = ask(
        &service,
        workspace_id,
        (&owner, &caller(&owner)),
        person_vault::LIST,
        Value::Null,
    )
    .await
    .unwrap();
    assert!(!listed.to_string().contains(TOKEN));
    let items = workspaces.open_vault(&workspace).await.unwrap().items();
    let uses = items
        .uses(&vault_items::ItemId::parse(token.id()).unwrap())
        .unwrap();
    assert_eq!(uses.len(), 2, "two releases, two uses: {uses:?}");
    assert_eq!(uses[0].by, BRIDGE, "the use names the app it went to");
    assert_eq!(uses[0].using, "tilo.vault/release");
    assert_eq!(uses[0].allowed, Some(vault_items::Allowance::Always));
    assert!(!format!("{uses:?}").contains(TOKEN));
    assert!(!format!("{:?}", items.facts()).contains(TOKEN));
    assert_eq!(
        files_holding(dir.path(), TOKEN.as_bytes()),
        Vec::<std::path::PathBuf>::new(),
        "no file holds the token in the clear"
    );
}

/// Every caller that is not a held connection of the app placed here is refused a release,
/// saying what is missing: the person and the pages they open, an agent, an app calling from a
/// process apps share, a window of the app or a process started for a call, a tilolang process,
/// and a connection another device holds.
#[tokio::test]
async fn every_other_caller_is_refused_a_release_saying_why() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let token = kept(&service, workspace_id, &owner, (GATEWAY, TOKEN)).await;
    let through_the_vault = |caller: Caller| {
        let service = &service;
        let owner = &owner;
        let token = &token;
        async move {
            ask_at(
                service,
                workspace_id,
                (owner, &caller),
                person_vault::release_target(token),
                Value::Null,
            )
            .await
        }
    };

    let person = caller(&owner).in_workspace(&workspace);
    assert_eq!(
        refusal(through_the_vault(person).await),
        release::BY_A_PERSON
    );
    let agent = on_turn(&owner, &workspace, "scout");
    assert_eq!(
        refusal(through_the_vault(agent).await),
        release::BY_AN_AGENT
    );
    let shared = Caller::local_app(BRIDGE)
        .acting_for(hex::encode(owner.user_public_key))
        .in_workspace(&workspace);
    assert_eq!(
        refusal(through_the_vault(shared).await),
        release::from_a_shared_process(BRIDGE)
    );

    let call = release_of(&token);
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &AppProcess::Unplaced, &call)
                .await
        ),
        release::from_an_unplaced_process(BRIDGE)
    );
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &AppProcess::Tilolang, &call)
                .await
        ),
        release::from_a_tilolang_process(BRIDGE)
    );
    // A process that says it is the connection, while this device holds no turn of it.
    let unheld = AppProcess::connection(BRIDGE);
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &unheld, &call)
                .await
        ),
        release::held_elsewhere(BRIDGE)
    );
    workspaces.run_connections(Arc::new(Bridge));
    takes_turn_in(
        workspaces,
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        AT_ONCE,
    )
    .await
    .expect("this device takes the bridge's connection");
    let unknown = ItemRef::parse("vault.item:0123456789abcdef0123456789abcdef").unwrap();
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &unheld, &release_of(&unknown))
                .await
        ),
        release::NOT_KEPT
    );
    let mut not_an_item = release_of(&token);
    not_an_item.target.address = ActionAddress::new(person_vault::ITEM_KIND, person_vault::RELEASE);
    assert!(matches!(
        workspaces
            .release_to((BRIDGE, &workspace), &unheld, &not_an_item)
            .await,
        Err(ActionCallError::InvalidRequest { reason }) if reason == release::NOT_AN_ITEM
    ));
}

/// The find of the item tagged for `app` and `purpose`, as the app's held connection calls it.
fn find_of(app: &str, purpose: &str) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: person_vault::PROVIDER.to_owned(),
            address: ActionAddress::new(person_vault::RESOURCE, person_vault::FIND),
        },
        argument: InvokeArgument::Json(
            json!({"app": app, "purpose": purpose})
                .as_object()
                .cloned()
                .unwrap(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A secret the old keeper held is adopted into the workspace's vault as the person, tagged
/// with its app and purpose: running it again answers the same item and adds nothing, only the
/// person's own device adopts, the app finds it by the tags it names and is asked once before
/// its release, and the secret is in no record, log or file in the clear.
#[tokio::test]
async fn a_kept_secret_is_adopted_once_tagged_found_and_asked_before_its_release() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _workspace_id, _service) = governed(workspaces).await;
    let person = hex::encode(owner.user_public_key);
    let (lines, _guard) = logged();

    let adopted = workspaces
        .adopt_secret(
            &workspace,
            &person,
            BRIDGE,
            "bot-token",
            "default",
            zeroize::Zeroizing::new(TOKEN.to_owned()),
        )
        .await
        .expect("the person's device adopts it");
    assert_eq!(
        workspaces
            .item_tagged(&workspace, BRIDGE, "bot-token")
            .await
            .unwrap(),
        Some(adopted.clone()),
        "the item is tagged for its app and purpose"
    );
    assert_eq!(
        workspaces
            .item_tagged(&workspace, BRIDGE, "webhook")
            .await
            .unwrap(),
        None
    );

    // Running it again answers the same item and keeps nothing second.
    let again = workspaces
        .adopt_secret(
            &workspace,
            &person,
            BRIDGE,
            "bot-token",
            "default",
            zeroize::Zeroizing::new(TOKEN.to_owned()),
        )
        .await
        .unwrap();
    assert_eq!(again, adopted);
    assert_eq!(
        workspaces
            .open_vault(&workspace)
            .await
            .unwrap()
            .items()
            .facts()
            .len(),
        1,
        "one item, not two"
    );

    // Only the person's own device adopts: another person's name adds nothing.
    let refused = workspaces
        .adopt_secret(
            &workspace,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            BRIDGE,
            "webhook",
            "default",
            zeroize::Zeroizing::new(OTHER.to_owned()),
        )
        .await;
    assert!(refused.is_err(), "a non-person does not adopt");
    assert_eq!(
        workspaces
            .item_tagged(&workspace, BRIDGE, "webhook")
            .await
            .unwrap(),
        None
    );

    workspaces.run_connections(Arc::new(Bridge));
    takes_turn_in(
        workspaces,
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        AT_ONCE,
    )
    .await
    .expect("this device takes the bridge's connection");
    let connection = AppProcess::connection(BRIDGE);
    let found = workspaces
        .find_tagged(
            (BRIDGE, &workspace),
            &connection,
            &find_of(BRIDGE, "bot-token"),
        )
        .await
        .expect("the bridge's held connection finds it");
    assert_eq!(found.reference, Some(adopted.clone()));
    assert!(
        workspaces
            .asked_store()
            .unwrap()
            .waiting_in(&workspace)
            .is_empty(),
        "a find asks the person nothing"
    );

    // Releasing what it found asks their person once, and the release answers the token.
    let asked = refusal(
        workspaces
            .release_to((BRIDGE, &workspace), &connection, &release_of(&adopted))
            .await,
    );
    assert_eq!(asked, release::asked_first(BRIDGE));
    let store = workspaces.asked_store().unwrap();
    let [waiting] = &store.waiting_in(&workspace)[..] else {
        panic!("one question");
    };
    let shown = asked_records(workspaces, &workspace);
    assert!(!shown.contains(TOKEN), "the question holds no secret");
    let answer = super::super::tests::settled(workspaces.resolve_asked(
        &caller(&owner).in_workspace(&workspace),
        &approve_and_grant(&waiting.asked.record),
    ))
    .await
    .expect("their answer is taken");
    assert!(matches!(answer.status, ReceiptStatus::Completed { .. }));
    let released: Released = workspaces
        .release_to((BRIDGE, &workspace), &connection, &release_of(&adopted))
        .await
        .expect("the allowed item is released");
    assert_eq!(released.secret, TOKEN);

    // The secret is in no log line and no file in the clear.
    assert!(
        !String::from_utf8_lossy(&lines.0.lock().unwrap()).contains(TOKEN),
        "the log holds no secret"
    );
    assert!(files_holding(dir.path(), TOKEN.as_bytes()).is_empty());
}

/// Two entries the old keeper held for one app and purpose, such as two seeds, are each kept:
/// every entry becomes its own item, and running one again answers the item it made.
#[tokio::test]
async fn entries_of_one_purpose_are_each_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _workspace_id, _service) = governed(workspaces).await;
    let person = hex::encode(owner.user_public_key);

    let first = workspaces
        .adopt_secret(
            &workspace,
            &person,
            BRIDGE,
            "seed",
            "first",
            zeroize::Zeroizing::new(TOKEN.to_owned()),
        )
        .await
        .unwrap();
    let second = workspaces
        .adopt_secret(
            &workspace,
            &person,
            BRIDGE,
            "seed",
            "second",
            zeroize::Zeroizing::new(OTHER.to_owned()),
        )
        .await
        .unwrap();
    assert_ne!(first, second, "each entry is its own item");
    assert_eq!(
        workspaces
            .open_vault(&workspace)
            .await
            .unwrap()
            .items()
            .facts()
            .len(),
        2,
        "both entries are kept"
    );
    assert_eq!(
        workspaces
            .adopt_secret(
                &workspace,
                &person,
                BRIDGE,
                "seed",
                "first",
                zeroize::Zeroizing::new(TOKEN.to_owned()),
            )
            .await
            .unwrap(),
        first,
        "the same entry answers the item it made"
    );
}

/// Another app, which keeps nothing of the bridge's.
const READER: &str = "so.tilo.reader";

/// The bridge and the reader, both installed here; neither runs a process.
struct BridgeAndReader;

impl HostConnections for BridgeAndReader {
    fn connections(&self) -> Vec<(String, Vec<String>)> {
        vec![
            (BRIDGE.to_owned(), Vec::new()),
            (READER.to_owned(), Vec::new()),
        ]
    }
    fn installed(&self, app: &str) -> bool {
        app == BRIDGE || app == READER
    }
    fn gives(&self, _: &str, _: &str, _: &str) -> bool {
        true
    }
    fn on_power(&self) -> bool {
        true
    }
    fn start(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn runs(&self, _: &str, _: &str) -> bool {
        false
    }
    fn stop(&self, _: &str, _: &str) {}
}

/// The keep of `value` for `purpose`, renewing `replacing` when named, as an app's held
/// connection calls it.
fn keep_of(purpose: &str, value: &str, replacing: Option<&ItemRef>) -> InvokeRequest {
    let mut argument = json!({"purpose": purpose, "value": value});
    if let Some(item) = replacing {
        argument["replacing"] = json!(item.to_string());
    }
    InvokeRequest {
        invocation: InvocationId::new(),
        target: person_vault::keep_target(),
        argument: InvokeArgument::Json(argument.as_object().cloned().unwrap()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The bridge's held connection keeps a token it was handed as its own: it is answered the
/// item's reference, keeping again renews that one item, and releasing it to the bridge asks
/// nobody. The person sees it among their items, titled and tagged. Another app is asked for it
/// first, is released it once the person approved and granted, and is asked again once they
/// took that leave back. The token is in no record, log line or file in the clear.
#[tokio::test]
async fn an_app_keeps_its_own_secret_renews_it_and_reads_it_back_unasked() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    workspaces.run_connections(Arc::new(BridgeAndReader));
    for app in [BRIDGE, READER] {
        takes_turn_in(
            workspaces,
            &workspace,
            &Turn::connection(app, Vec::new()),
            AT_ONCE,
        )
        .await
        .expect("this device takes the app's connection");
    }
    let bridge = AppProcess::connection(BRIDGE);
    let reader = AppProcess::connection(READER);
    let (lines, guard) = logged();
    tracing::info!("the line that shows the log is read");

    // The value is a secret of the published action, so no record of a call keeps it.
    let described = crate::workspaces::actions::published_params(&person_vault::keep_target())
        .expect("vault#keep is described");
    assert_eq!(
        tilo_actions::remote::secret_arguments::secret_parameters(&described),
        vec!["value".to_owned()]
    );

    let kept = workspaces
        .keep_for(
            (BRIDGE, &workspace),
            &bridge,
            &keep_of("bot-token", OTHER, None),
        )
        .await
        .expect("the bridge keeps its own token");
    let store = workspaces.asked_store().unwrap();
    assert!(
        store.waiting_in(&workspace).is_empty(),
        "keeping asks nobody"
    );

    // Keeping again for the purpose renews the one item, and so does naming it.
    let renewed = workspaces
        .keep_for(
            (BRIDGE, &workspace),
            &bridge,
            &keep_of("bot-token", OTHER, None),
        )
        .await
        .unwrap();
    assert_eq!(renewed.reference, kept.reference);
    let named = workspaces
        .keep_for(
            (BRIDGE, &workspace),
            &bridge,
            &keep_of("bot-token", TOKEN, Some(&kept.reference)),
        )
        .await
        .unwrap();
    assert_eq!(named.reference, kept.reference);
    assert_eq!(
        workspaces
            .item_tagged(&workspace, BRIDGE, "bot-token")
            .await
            .unwrap(),
        Some(kept.reference.clone()),
        "the bridge finds it by its purpose"
    );

    // The person sees it as one item of their vault, titled for the app and the purpose.
    let mine = (&owner, &caller(&owner));
    let listed = ask(
        &service,
        workspace_id,
        mine,
        person_vault::LIST,
        Value::Null,
    )
    .await
    .unwrap();
    let [item] = &listed["items"].as_array().unwrap()[..] else {
        panic!("one item, renewed in place: {listed}");
    };
    assert_eq!(item["reference"], kept.reference.to_string());
    assert_eq!(item["kind"], "value");
    assert_eq!(item["title"], format!("{BRIDGE} · bot token"));
    assert_eq!(
        item["tags"],
        json!([
            person_vault::app_tag(BRIDGE),
            person_vault::purpose_tag("bot-token")
        ])
    );

    // The bridge reads back the newest value, asking nobody.
    let call = release_of(&kept.reference);
    let released: Released = workspaces
        .release_to((BRIDGE, &workspace), &bridge, &call)
        .await
        .expect("the bridge reads what it kept");
    assert_eq!(released.secret, TOKEN);
    assert!(
        store.waiting_in(&workspace).is_empty(),
        "its own release asks nobody"
    );

    // Another app is asked for it first; approved and granted, it reads it.
    assert_eq!(
        refusal(
            workspaces
                .release_to((READER, &workspace), &reader, &call)
                .await
        ),
        release::asked_first(READER)
    );
    let [waiting] = &store.waiting_in(&workspace)[..] else {
        panic!("one question");
    };
    assert!(!asked_records(workspaces, &workspace).contains(TOKEN));
    super::super::tests::settled(workspaces.resolve_asked(
        &caller(&owner).in_workspace(&workspace),
        &approve_and_grant(&waiting.asked.record),
    ))
    .await
    .expect("their answer is taken");
    let lent: Released = workspaces
        .release_to((READER, &workspace), &reader, &call)
        .await
        .expect("the reader reads what it was lent");
    assert_eq!(lent.secret, TOKEN);

    // The person sees the reader's leave and takes it back: the reader is asked again, the
    // bridge still is not.
    let read = ask(
        &service,
        workspace_id,
        mine,
        person_vault::USES,
        json!({"item": kept.reference.to_string()}),
    )
    .await
    .unwrap();
    let leaves = read["always"].as_array().cloned().unwrap_or_default();
    let [leave] = &leaves[..] else {
        panic!("the reader's leave is listed: {read}");
    };
    assert_eq!(leave["app"], READER);
    ask(
        &service,
        workspace_id,
        mine,
        person_vault::TAKE_BACK,
        json!({"item": kept.reference.to_string(), "app": READER}),
    )
    .await
    .expect("the person takes the reader's leave back");
    assert_eq!(
        refusal(
            workspaces
                .release_to((READER, &workspace), &reader, &call)
                .await
        ),
        release::asked_first(READER)
    );
    workspaces
        .release_to((BRIDGE, &workspace), &bridge, &call)
        .await
        .expect("the bridge still reads its own");

    // Neither value is in a record, a use, a log line or a file in the clear.
    drop(guard);
    let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("the line that shows the log is read"));
    let items = workspaces.open_vault(&workspace).await.unwrap().items();
    let uses = items
        .uses(&vault_items::ItemId::parse(kept.reference.id()).unwrap())
        .unwrap();
    assert!(
        uses.iter()
            .any(|used| used.by == BRIDGE && used.allowed.is_none()),
        "the bridge's own release was nobody's leave: {uses:?}"
    );
    for secret in [TOKEN, OTHER] {
        assert!(!logged.contains(secret), "a log line holds it: {logged}");
        assert!(!asked_records(workspaces, &workspace).contains(secret));
        assert!(!listed.to_string().contains(secret));
        assert!(!format!("{uses:?}").contains(secret));
        assert!(!format!("{:?}", items.facts()).contains(secret));
        assert_eq!(
            files_holding(dir.path(), secret.as_bytes()),
            Vec::<std::path::PathBuf>::new(),
            "no file holds it in the clear"
        );
    }
}

/// Every caller that is not an app's held connection placed here is refused a keep, saying why,
/// as a release is: the person and the pages they open, an agent, an app calling from a process
/// apps share, a window or a process started for a call, a tilolang process, and a connection
/// another device holds. A held connection is refused renewing an item it did not keep, and a
/// keep that names no purpose.
#[tokio::test]
async fn every_other_caller_is_refused_a_keep_saying_why() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let persons = kept(&service, workspace_id, &owner, (GATEWAY, OTHER)).await;
    let through_the_vault = |caller: Caller| {
        let service = &service;
        let owner = &owner;
        async move {
            ask_at(
                service,
                workspace_id,
                (owner, &caller),
                person_vault::keep_target(),
                json!({"purpose": "bot-token", "value": TOKEN}),
            )
            .await
        }
    };
    let person = caller(&owner).in_workspace(&workspace);
    assert_eq!(
        refusal(through_the_vault(person).await),
        release::BY_A_PERSON
    );
    let agent = on_turn(&owner, &workspace, "scout");
    assert_eq!(
        refusal(through_the_vault(agent).await),
        release::BY_AN_AGENT
    );
    let shared = Caller::local_app(BRIDGE)
        .acting_for(hex::encode(owner.user_public_key))
        .in_workspace(&workspace);
    assert_eq!(
        refusal(through_the_vault(shared).await),
        release::from_a_shared_process(BRIDGE)
    );

    let call = keep_of("bot-token", TOKEN, None);
    assert_eq!(
        refusal(
            workspaces
                .keep_for((BRIDGE, &workspace), &AppProcess::Unplaced, &call)
                .await
        ),
        release::from_an_unplaced_process(BRIDGE)
    );
    assert_eq!(
        refusal(
            workspaces
                .keep_for((BRIDGE, &workspace), &AppProcess::Tilolang, &call)
                .await
        ),
        release::from_a_tilolang_process(BRIDGE)
    );
    // A process that says it is the connection, while this device holds no turn of it.
    let held = AppProcess::connection(BRIDGE);
    assert_eq!(
        refusal(
            workspaces
                .keep_for((BRIDGE, &workspace), &held, &call)
                .await
        ),
        release::held_elsewhere(BRIDGE)
    );

    workspaces.run_connections(Arc::new(Bridge));
    takes_turn_in(
        workspaces,
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        AT_ONCE,
    )
    .await
    .expect("this device takes the bridge's connection");
    assert_eq!(
        refusal(
            workspaces
                .keep_for(
                    (BRIDGE, &workspace),
                    &held,
                    &keep_of("bot-token", TOKEN, Some(&persons)),
                )
                .await
        ),
        release::not_its_own(BRIDGE),
        "the person's own item is not the bridge's to renew"
    );
    assert_eq!(
        refusal(
            workspaces
                .keep_for((BRIDGE, &workspace), &held, &keep_of(" ", TOKEN, None))
                .await
        ),
        release::NOT_A_PURPOSE
    );
    // The person's item is still theirs alone, and nothing of the bridge's was kept.
    let facts = workspaces
        .open_vault(&workspace)
        .await
        .unwrap()
        .items()
        .facts();
    assert_eq!(facts.len(), 1);
    assert!(facts[0].tags.is_empty());
    assert!(!asked_records(workspaces, &workspace).contains(TOKEN));
    assert!(files_holding(dir.path(), TOKEN.as_bytes()).is_empty());
}

/// The tilolang process of the bridge's own instances keeps its token and reads it back with
/// nobody asked, and each call stands in the device's record of invocations without the token.
/// It is refused every item the bridge did not keep itself: the person's, and the reader's even
/// once the person lent it to the bridge's held connection. A tilolang process apps share and a
/// process no turn placed are refused both keeping and reading back.
#[tokio::test]
async fn a_tilolang_process_of_the_apps_own_keeps_and_reads_back_only_what_it_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let records = Arc::new(tilo_actions::remote::invocation_record::Invocations::default());
    records.keep_no_secrets(workspaces.secret_parameters());
    workspaces.serve_invocations(Arc::clone(&records));
    let persons = kept(&service, workspace_id, &owner, (GATEWAY, OTHER)).await;
    let (lines, guard) = logged();
    let own = AppProcess::OwnTilolang;

    let call = keep_of("bot-token", TOKEN, None);
    let kept_here = workspaces
        .keep_for((BRIDGE, &workspace), &own, &call)
        .await
        .expect("the bridge's own tilolang process keeps its token");
    let release = release_of(&kept_here.reference);
    let released: Released = workspaces
        .release_to((BRIDGE, &workspace), &own, &release)
        .await
        .expect("and reads it back");
    assert_eq!(released.secret, TOKEN);
    let store = workspaces.asked_store().unwrap();
    assert!(store.waiting_in(&workspace).is_empty(), "nobody was asked");

    // Both calls are in the device's record, as the bridge's, without the token.
    let listed = records.listed(&workspace);
    let record_of = |id: InvocationId| {
        listed
            .iter()
            .find(|recorded| recorded.request.invocation == id)
            .cloned()
    };
    let kept_record = record_of(call.invocation).expect("the keep is recorded");
    assert_eq!(kept_record.request.target, person_vault::keep_target());
    assert_eq!(kept_record.sealed, vec!["value".to_owned()]);
    assert_eq!(
        kept_record.caller.acting().app().map(|app| app.to_string()),
        Some(BRIDGE.to_owned())
    );
    assert!(
        matches!(
            kept_record.state.as_ref().map(|state| &state.update),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ),
        "the keep ended: {:?}",
        kept_record.state
    );
    let release_record = record_of(release.invocation).expect("the release is recorded");
    assert!(
        release_record.secret,
        "it keeps only that the secret was given"
    );
    let recorded: String = listed
        .iter()
        .map(|recorded| {
            format!(
                "{}{:?}",
                serde_json::to_string(&recorded.request).unwrap(),
                recorded.state
            )
        })
        .collect();
    assert!(recorded.contains("bot-token"), "{recorded}");
    assert!(
        !recorded.contains(TOKEN),
        "a record holds the token: {recorded}"
    );

    // The bridge's tilolang code never reads it back: its calls reach the vault through the
    // app's broker, as a process apps share, and a release or a keep there is refused. Only the
    // process's Rust host, on the daemon's line for that very process, is answered.
    let tilolang_code = Caller::local_app(BRIDGE)
        .acting_for(hex::encode(owner.user_public_key))
        .in_workspace(&workspace);
    for (target, argument) in [
        (
            person_vault::release_target(&kept_here.reference),
            Value::Null,
        ),
        (
            person_vault::keep_target(),
            json!({"purpose": "bot-token", "value": OTHER}),
        ),
    ] {
        assert_eq!(
            refusal(
                ask_at(
                    &service,
                    workspace_id,
                    (&owner, &tilolang_code),
                    target,
                    argument,
                )
                .await
            ),
            release::from_a_shared_process(BRIDGE)
        );
    }

    // The person's own item is not the bridge's.
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &own, &release_of(&persons))
                .await
        ),
        release::not_kept_by_the_app(BRIDGE)
    );

    // The reader's item, lent to the bridge's held connection, is still not its tilolang
    // process's.
    workspaces.run_connections(Arc::new(BridgeAndReader));
    takes_turn_in(
        workspaces,
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        AT_ONCE,
    )
    .await
    .expect("this device takes the bridge's connection");
    let readers = workspaces
        .keep_for(
            (READER, &workspace),
            &AppProcess::OwnTilolang,
            &keep_of("api-key", OTHER, None),
        )
        .await
        .unwrap();
    let lend = release_of(&readers.reference);
    let held = AppProcess::connection(BRIDGE);
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &held, &lend)
                .await
        ),
        release::asked_first(BRIDGE)
    );
    let [waiting] = &store.waiting_in(&workspace)[..] else {
        panic!("one question");
    };
    super::super::tests::settled(workspaces.resolve_asked(
        &caller(&owner).in_workspace(&workspace),
        &approve_and_grant(&waiting.asked.record),
    ))
    .await
    .expect("their answer is taken");
    let lent: Released = workspaces
        .release_to((BRIDGE, &workspace), &held, &lend)
        .await
        .expect("the held connection reads what it was lent");
    assert_eq!(lent.secret, OTHER);
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &own, &lend)
                .await
        ),
        release::not_kept_by_the_app(BRIDGE),
        "a lent item never reaches a tilolang process"
    );

    // A tilolang process apps share, and a process no turn placed, are refused both.
    for (process, why) in [
        (
            AppProcess::Tilolang,
            release::from_a_tilolang_process(BRIDGE),
        ),
        (
            AppProcess::Unplaced,
            release::from_an_unplaced_process(BRIDGE),
        ),
    ] {
        assert_eq!(
            refusal(
                workspaces
                    .keep_for(
                        (BRIDGE, &workspace),
                        &process,
                        &keep_of("bot-token", TOKEN, None)
                    )
                    .await
            ),
            why
        );
        assert_eq!(
            refusal(
                workspaces
                    .release_to((BRIDGE, &workspace), &process, &release)
                    .await
            ),
            why
        );
    }

    drop(guard);
    let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
    for secret in [TOKEN, OTHER] {
        assert!(!logged.contains(secret), "a log line holds it: {logged}");
        assert!(!asked_records(workspaces, &workspace).contains(secret));
        assert!(files_holding(dir.path(), secret.as_bytes()).is_empty());
    }
}

/// A read given once ([`tilo_action_policy::Lifetime::Once`]) releases the item once: the grant
/// is taken back as it is used, and the next release asks the person again.
#[tokio::test]
async fn a_read_given_once_releases_the_item_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let token = kept(&service, workspace_id, &owner, (GATEWAY, TOKEN)).await;
    workspaces.run_connections(Arc::new(Bridge));
    takes_turn_in(
        workspaces,
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        AT_ONCE,
    )
    .await
    .expect("this device takes the bridge's connection");
    let connection = AppProcess::connection(BRIDGE);

    let once = tilo_action_policy::Grant::new(
        "bridge-reads-once",
        tilo_action_policy::Grantee::app(BRIDGE),
        release::read_node(&workspace, &token),
        tilo_action_policy::KindSet::of(tilo_action_policy::ActionKind::Read),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(hex::encode(owner.user_public_key)),
    )
    .with_lifetime(tilo_action_policy::Lifetime::Once);
    workspaces
        .put_workspace_grants(&workspace, &owner, [0x71; 32], vec![once])
        .await
        .expect("the person's grant is written");

    let released: Released = workspaces
        .release_to((BRIDGE, &workspace), &connection, &release_of(&token))
        .await
        .expect("the read given once releases the item");
    assert_eq!(released.secret, TOKEN);
    let policy = workspaces.policy_of(&workspace).await.unwrap();
    assert!(
        !policy
            .grants
            .iter()
            .any(|grant| grant.id.as_str() == "bridge-reads-once"),
        "the grant is spent by its one use"
    );
    assert_eq!(
        refusal(
            workspaces
                .release_to((BRIDGE, &workspace), &connection, &release_of(&token))
                .await
        ),
        release::asked_first(BRIDGE),
        "the next release asks again"
    );
}
