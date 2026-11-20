//! A person's vault on their server: what they keep to sign in with is added,
//! listed and removed only from their own device, a listing says only what
//! is safe to say, and no agent reaches any of it, whoever it acts for.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the server's handle a test binds is kept until the test ends; dropping it earlier closes the server it drives"
)]

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::remote::{person_accounts, person_models, person_vault};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, caller, governed, settled};
use crate::workspaces::fixtures::test_server_keeping_keys_apart;
use crate::workspaces::tests::{governor, test_host};

const PASSWORD: &str = "correct horse battery staple";
const NAME: &str = "ada@example.test";
/// What no site shows as a setup key: it holds digits a seed never does.
const NOT_A_SEED: &str = "correct horse battery staple 0189";
const KEY: &str = "sk-live-ada-own-key";

/// `target` asked by `caller`, on the device `device` certifies, in the
/// workspace `workspace_id` carries.
pub(super) async fn ask_at(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    target: RemoteActionTarget,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let receipt = settled(
        service
            .in_workspace_as(workspace_id, device)
            .invoke(caller, request),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a person's own vault answers at once: {other:?}"),
    }
}

/// `action` of the vault, asked as [`ask_at`] asks.
pub(super) async fn ask(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    ask_at(
        service,
        workspace_id,
        who,
        person_vault::target(action),
        argument,
    )
    .await
}

/// The fields `said` carries, in order of their names.
fn fields(said: &Value) -> Vec<&str> {
    let mut fields: Vec<&str> = said
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    fields
}

/// What `other` reaches of the item `reference` names in someone else's
/// vault: nothing listed, nothing removed.
async fn reaches_nothing_of_it(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    other: (&DeviceCert, &Caller),
    reference: &str,
) {
    let theirs = ask(service, workspace_id, other, person_vault::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(theirs["items"], json!([]), "{theirs}");
    let refused = ask(
        service,
        workspace_id,
        other,
        person_vault::REMOVE,
        json!({"item": reference}),
    )
    .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason == "That item is not kept here"),
        "{refused:?}"
    );
}

/// A login to keep for the site `site`.
fn a_login(site: &str) -> Value {
    json!({"kind": "login", "belongsTo": [site], "username": NAME, "secret": PASSWORD})
}

/// Every file under `dir` whose bytes hold `needle`.
pub(super) fn files_holding(dir: &std::path::Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => waiting.push(path),
                Ok(metadata) if metadata.is_file() => {
                    let bytes = std::fs::read(&path).unwrap();
                    if bytes.windows(needle.len()).any(|window| window == needle) {
                        holding.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    holding
}

/// A person keeps a login from their own device. What the server answers,
/// then and in every listing, is its kind, where it belongs and when it was
/// added, under a reference: never the secret, never the name. Another
/// member, the workspace's owner included, lists nothing of it and removes
/// nothing of it, and the host's disk holds it nowhere in the clear.
#[tokio::test]
async fn a_person_keeps_what_they_sign_in_with_and_reads_back_only_what_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo, as_owner) = (caller(&ada), caller(&bo), caller(&owner));

    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        a_login("https://www.linkedin.com/login?session=1"),
    )
    .await
    .expect("Ada keeps a login");
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    assert!(
        person_vault::ItemRef::parse(&reference).is_some(),
        "{reference}"
    );
    assert_eq!(kept["kind"], "login");
    assert_eq!(kept["belongsTo"], json!(["https://www.linkedin.com"]));
    assert!(kept["addedAt"].as_u64().is_some_and(|added| added > 0));
    assert_eq!(fields(&kept), ["addedAt", "belongsTo", "kind", "reference"]);

    let hers = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(hers["items"], json!([kept]));
    for said in [&kept, &hers] {
        let said = said.to_string();
        assert!(!said.contains(PASSWORD) && !said.contains(NAME), "{said}");
    }

    for other in [(&bo, &as_bo), (&owner, &as_owner)] {
        reaches_nothing_of_it(&service, workspace_id, other, &reference).await;
    }

    for needle in [PASSWORD, NAME, "linkedin"] {
        let clear = files_holding(dir.path(), needle.as_bytes());
        assert!(clear.is_empty(), "{needle} in the clear: {clear:?}");
    }
    let own = workspaces.person_settings_root(&ada.user_public_key);
    assert!(
        accounts::vault_dir_in(&own).join("vault.enc").exists(),
        "the item is sealed in Ada's own vault"
    );

    let remaining = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::REMOVE,
        json!({"item": reference}),
    )
    .await
    .expect("Ada forgets her login");
    assert_eq!(remaining["items"], json!([]));
    server.shutdown().await;
}

/// No agent lists a person's vault, adds to it or removes from it: not one
/// certified under the person's own key, not one their own device handed
/// its connection to, not a bot, not an app. Neither does the person's key
/// on a device that is not the one the certificate names. Each is told what
/// a caller asking for something that does not exist is told.
#[tokio::test]
async fn no_agent_reaches_a_persons_vault() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let reference = kept["reference"].clone();

    let person = hex::encode(ada.user_public_key);
    let not_her: [(&str, &DeviceCert, Caller); 6] = [
        (
            "an agent certified under her key",
            &ada,
            Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key),
        ),
        (
            "an agent her own device handed its connection to",
            &ada,
            caller(&ada).delegated_to("assistant"),
        ),
        (
            "a bot she installed",
            &ada,
            Caller::bot("bot", ada.user_public_key, ada.device_public_key),
        ),
        (
            "an agent the server admitted itself, acting for her",
            &ada,
            Caller::local_agent("assistant").acting_for(person.clone()),
        ),
        (
            "an app acting for her",
            &ada,
            Caller::local_app("so.tilo.chat").acting_for(person),
        ),
        ("her key on another person's device", &bo, caller(&ada)),
    ];
    for (who, device, asking) in &not_her {
        for (action, argument) in [
            (person_vault::LIST, json!({})),
            (person_vault::ADD, a_login("https://attacker.example")),
            (person_vault::REMOVE, json!({"item": reference})),
            (person_vault::ENTRIES, json!({})),
            (
                person_vault::KEEP_ENTRY,
                json!({"entry": an_entry("Planted", "planted")}),
            ),
            (
                person_vault::FORGET_ENTRY,
                json!({"id": "an-entry-of-hers"}),
            ),
        ] {
            let refused = ask(&service, workspace_id, (device, asking), action, argument).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{who}, {action}: {refused:?}"
            );
        }
    }

    // Nothing of hers moved, and nothing was kept for anyone else.
    let hers = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(hers["items"], json!([kept]));
    let his = ask(
        &service,
        workspace_id,
        (&bo, &caller(&bo)),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(his["items"], json!([]));
    server.shutdown().await;
}

/// An entry of the person's Passwords: what the app sends, secret included.
fn an_entry(title: &str, password: &str) -> Value {
    let mut entry = accounts::SecretItem::login(title, NAME, password);
    entry.add_origin("https://example.com").unwrap();
    serde_json::to_value(entry).unwrap()
}

/// The person keeps the entries of their Passwords in their vault from their
/// own device, reads them back secrets included, edits one in place and
/// forgets it. Another person's device reads none of it.
#[tokio::test]
async fn a_persons_passwords_are_kept_and_read_back_by_them_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let hers = (&ada, &caller(&ada));
    let entries = |answer: &Value| answer["entries"].as_array().cloned().unwrap_or_default();

    let entry = an_entry("Bank", PASSWORD);
    let kept = ask(
        &service,
        workspace_id,
        hers,
        person_vault::KEEP_ENTRY,
        json!({"entry": entry}),
    )
    .await
    .expect("Ada keeps an entry");
    assert_eq!(entries(&kept).len(), 1, "{kept}");
    let read = ask(
        &service,
        workspace_id,
        hers,
        person_vault::ENTRIES,
        json!({}),
    )
    .await
    .expect("Ada reads her entries");
    let read = entries(&read);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0]["title"], "Bank");
    assert_eq!(
        read[0]["password"], PASSWORD,
        "her own secret comes back to her"
    );

    let mut edited = read[0].clone();
    edited["title"] = json!("Bank, renamed");
    let after = ask(
        &service,
        workspace_id,
        hers,
        person_vault::KEEP_ENTRY,
        json!({"entry": edited}),
    )
    .await
    .unwrap();
    let after = entries(&after);
    assert_eq!(after.len(), 1, "edited in place");
    assert_eq!(after[0]["title"], "Bank, renamed");

    let his = ask(
        &service,
        workspace_id,
        (&bo, &caller(&bo)),
        person_vault::ENTRIES,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(his["entries"], json!([]), "Bo reads none of hers");
    // Her entries are no items an agent may use, and the other way round.
    let items = ask(&service, workspace_id, hers, person_vault::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(items["items"], json!([]));

    let id = after[0]["id"].clone();
    let left = ask(
        &service,
        workspace_id,
        hers,
        person_vault::FORGET_ENTRY,
        json!({"id": id}),
    )
    .await
    .unwrap();
    assert_eq!(left["entries"], json!([]));
    let again = ask(
        &service,
        workspace_id,
        hers,
        person_vault::FORGET_ENTRY,
        json!({"id": id}),
    )
    .await;
    assert!(again.is_ok(), "one gone already is no refusal: {again:?}");
    server.shutdown().await;
}

/// An item that is not one to keep is refused before anything is kept, and
/// the refusal never repeats what was typed: a secret put where a place or a
/// kind belongs is not echoed back.
#[tokio::test]
async fn an_item_that_is_not_one_is_refused_without_repeating_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);

    for unusable in [
        json!({"kind": "login", "belongsTo": [PASSWORD], "secret": PASSWORD}),
        json!({"kind": PASSWORD, "belongsTo": ["https://example.com"], "secret": PASSWORD}),
        json!({"kind": "login", "belongsTo": PASSWORD, "secret": PASSWORD}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "secret": [PASSWORD]}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "password": PASSWORD}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "secret": ""}),
        json!({"kind": "login", "belongsTo": [], "secret": PASSWORD}),
        json!({"kind": "ssh-key", "belongsTo": ["https://example.com"], "secret": PASSWORD}),
        json!({"kind": "code-generator", "belongsTo": ["https://example.com"], "secret": NOT_A_SEED}),
    ] {
        let refused = ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            person_vault::ADD,
            unusable.clone(),
        )
        .await
        .expect_err("an unusable item is refused");
        assert!(
            matches!(
                refused,
                ActionCallError::Refused { .. } | ActionCallError::InvalidRequest { .. }
            ),
            "{unusable}: {refused:?}"
        );
        let said = format!("{refused} {refused:?}");
        assert!(!said.contains(PASSWORD) && !said.contains("0189"), "{said}");
    }
    let refused = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::REMOVE,
        json!({"item": PASSWORD}),
    )
    .await
    .expect_err("what is no reference names nothing");
    assert!(!format!("{refused} {refused:?}").contains(PASSWORD));

    let hers = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(hers["items"], json!([]));
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// A provider of whoever asks, at `https://models.example/v1`, by its id.
pub(super) async fn a_provider(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> String {
    let added = ask_at(
        service,
        workspace_id,
        who,
        person_models::target(person_models::ADD_PROVIDER),
        json!({"name": "Models", "protocol": "open-ai-compatible", "endpoint": "https://models.example/v1"}),
    )
    .await
    .expect("a provider is added");
    added["id"].as_str().expect("the provider's id").to_owned()
}

/// A `kind` kept for `place` in the vault of whoever asks, by its reference.
pub(super) async fn kept_item(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    (kind, place): (&str, &str),
) -> Value {
    let kept = ask(
        service,
        workspace_id,
        who,
        person_vault::ADD,
        json!({"kind": kind, "belongsTo": [place], "secret": KEY}),
    )
    .await
    .expect("an item is kept");
    kept["reference"].clone()
}

/// The uses the item `item` names remembers, as its person reads them.
async fn uses_of(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    item: &Value,
) -> Value {
    let uses = ask(
        service,
        workspace_id,
        who,
        person_vault::USES,
        json!({"item": item}),
    )
    .await
    .expect("a person reads what their item was used for");
    uses["uses"].clone()
}

/// An agent acting for a person keeps a provider's key from their vault by
/// naming the item. The host resolves it: the provider is configured, the
/// item remembers when, where and for whom, and nothing the agent is
/// answered carries the key. What the item was used for is the person's to
/// read, not the agent's.
#[tokio::test]
async fn an_agent_keeps_a_providers_key_by_naming_the_vault_item() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let provider = a_provider(&service, workspace_id, (&ada, &as_ada)).await;
    let item = kept_item(
        &service,
        workspace_id,
        (&ada, &as_ada),
        ("key", "https://models.example/v1/chat"),
    )
    .await;
    let helper = Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key)
        .named("Helper");

    let view = ask_at(
        &service,
        workspace_id,
        (&ada, &helper),
        person_models::target(person_models::SET_KEY_FROM_ITEM),
        json!({"provider": provider, "key": item}),
    )
    .await
    .expect("the agent keeps the key by naming its item");
    assert_eq!(view["configured"], json!(true), "{view}");
    assert!(!view.to_string().contains(KEY), "{view}");

    // The key is the provider's now: sealed in Ada's vault, nowhere in the
    // clear.
    let own = workspaces.person_settings_root(&ada.user_public_key);
    let (reference, authorization) =
        accounts::provider_api_key(&provider, "https://models.example/v1").unwrap();
    let sealed = accounts::ServiceCredentials::open_in(&own)
        .resolve(&reference, &authorization)
        .unwrap()
        .expect("the provider's key is kept");
    assert_eq!(sealed.as_str(), KEY);
    assert!(files_holding(dir.path(), KEY.as_bytes()).is_empty());

    let listed = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert!(
        listed["items"][0]["lastUsedAt"].as_u64().is_some(),
        "{listed}"
    );
    let uses = uses_of(&service, workspace_id, (&ada, &as_ada), &item).await;
    assert_eq!(uses.as_array().map(Vec::len), Some(1), "{uses}");
    assert_eq!(uses[0]["places"], json!(["https://models.example"]));
    assert_eq!(uses[0]["by"], "Helper");
    assert_eq!(uses[0]["using"], "tilo.models/set-key-from-item");
    assert_eq!(fields(&uses[0]), ["at", "by", "places", "using"]);
    let refused = ask(
        &service,
        workspace_id,
        (&ada, &helper),
        person_vault::USES,
        json!({"item": item}),
    )
    .await;
    assert!(
        matches!(refused, Err(ActionCallError::UnknownAction)),
        "{refused:?}"
    );
    server.shutdown().await;
}

/// A provider of whoever asks and three items of theirs a use might name:
/// a key for another service, a login for the provider's site, and the
/// provider's own key.
async fn a_provider_and_items(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> (String, [Value; 3]) {
    let provider = a_provider(service, workspace_id, who).await;
    let elsewhere = kept_item(service, workspace_id, who, ("key", "https://other.example")).await;
    let login = kept_item(
        service,
        workspace_id,
        who,
        ("login", "https://models.example"),
    )
    .await;
    let right = kept_item(
        service,
        workspace_id,
        who,
        ("key", "https://models.example"),
    )
    .await;
    (provider, [elsewhere, login, right])
}

/// Nothing was kept and nothing used: the provider of `who` has no key and
/// none of `items` remembers a use.
async fn nothing_was_used(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    items: &[Value; 3],
) {
    let models = ask_at(
        service,
        workspace_id,
        who,
        person_models::target(person_models::LIST),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        models["providers"][0]["configured"],
        json!(false),
        "{models}"
    );
    for item in items {
        assert_eq!(uses_of(service, workspace_id, who, item).await, json!([]));
    }
}

/// A reference is used only where its item belongs, as the kind the use
/// needs, for the person whose vault holds it: the wrong place, the wrong
/// kind and another person's vault are each refused, and nothing is used.
#[tokio::test]
async fn a_reference_is_refused_wherever_its_item_does_not_belong() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));
    let hers = (&ada, &as_ada);
    let (provider, items) = a_provider_and_items(&service, workspace_id, hers).await;
    let [elsewhere, login, right] = &items;
    let his_provider = a_provider(&service, workspace_id, (&bo, &as_bo)).await;
    let helper = Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key);
    let his_helper = Caller::delegated_agent("assistant", bo.user_public_key, bo.device_public_key);

    for (who, provider, item, reason) in [
        (
            (&ada, &helper),
            &provider,
            elsewhere,
            "That item belongs somewhere else",
        ),
        (
            (&ada, &helper),
            &provider,
            login,
            "That item is not the kind of thing this needs",
        ),
        (
            (&bo, &his_helper),
            &his_provider,
            right,
            "That item is not kept here",
        ),
        (
            (&bo, &his_helper),
            &provider,
            right,
            "That provider is not kept here",
        ),
    ] {
        let refused = ask_at(
            &service,
            workspace_id,
            who,
            person_models::target(person_models::SET_KEY_FROM_ITEM),
            json!({"provider": provider, "key": item}),
        )
        .await;
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { reason: said }) if said == reason),
            "{reason}: {refused:?}"
        );
    }
    nothing_was_used(&service, workspace_id, hers, &items).await;
    server.shutdown().await;
}

/// A bot, an app and an agent no certificate stands behind use nothing of a
/// person's vault, and the person's own agent hands no typed key in: that
/// way stays the person's own.
#[tokio::test]
async fn only_a_persons_own_agent_uses_their_vault_and_only_by_reference() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let (provider, items) = a_provider_and_items(&service, workspace_id, hers).await;
    let [_, _, right] = &items;
    let from_item = person_models::target(person_models::SET_KEY_FROM_ITEM);
    let person = hex::encode(ada.user_public_key);
    let helper = Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key);

    for (other, target, key) in [
        (
            Caller::bot("bot", ada.user_public_key, ada.device_public_key),
            from_item.clone(),
            right.clone(),
        ),
        (
            Caller::local_app("so.tilo.chat").acting_for(person.clone()),
            from_item.clone(),
            right.clone(),
        ),
        (
            Caller::local_agent("assistant").acting_for(person),
            from_item,
            right.clone(),
        ),
        (
            helper,
            person_models::target(person_models::SET_KEY),
            json!(KEY),
        ),
    ] {
        let refused = ask_at(
            &service,
            workspace_id,
            (&ada, &other),
            target,
            json!({"provider": provider, "key": key}),
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::UnknownAction)),
            "{other:?}: {refused:?}"
        );
    }
    nothing_was_used(&service, workspace_id, hers, &items).await;
    server.shutdown().await;
}

/// A mail sign-in named by reference goes only to the servers its item
/// belongs to: an account at any other server or port is refused before a
/// server is reached, and typing a password in stays the person's own.
#[tokio::test]
async fn a_mail_sign_in_by_reference_goes_only_to_its_own_servers() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let kept = ask(
        &service,
        workspace_id,
        hers,
        person_vault::ADD,
        json!({
            "kind": "login",
            "belongsTo": ["imap://imap.example.test:993", "smtp://smtp.example.test:465"],
            "username": NAME,
            "secret": PASSWORD,
        }),
    )
    .await
    .expect("Ada keeps her mail sign-in");
    let sign_in = kept["reference"].clone();
    let helper = Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key);
    let with_item = person_accounts::target(person_accounts::ADD_MAIL_WITH_ITEM);

    for (incoming, port, outgoing) in [
        ("imap.attacker.example", 993, "smtp.example.test"),
        ("imap.example.test", 993, "smtp.attacker.example"),
        ("imap.example.test", 143, "smtp.example.test"),
    ] {
        let refused = ask_at(
            &service,
            workspace_id,
            (&ada, &helper),
            with_item.clone(),
            json!({
                "address": NAME,
                "incomingHost": incoming,
                "incomingPort": port,
                "outgoingHost": outgoing,
                "password": sign_in,
            }),
        )
        .await;
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { reason }) if reason == "That item belongs somewhere else"),
            "{incoming}:{port} {outgoing}: {refused:?}"
        );
    }
    let typed = ask_at(
        &service,
        workspace_id,
        (&ada, &helper),
        person_accounts::target(person_accounts::ADD_MAIL),
        json!({
            "address": NAME,
            "incomingHost": "imap.example.test",
            "outgoingHost": "smtp.example.test",
            "password": PASSWORD,
        }),
    )
    .await;
    assert!(
        matches!(typed, Err(ActionCallError::UnknownAction)),
        "{typed:?}"
    );

    let accounts = ask_at(
        &service,
        workspace_id,
        hers,
        person_accounts::target(person_accounts::LIST),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(accounts["accounts"], json!([]), "{accounts}");
    assert_eq!(
        uses_of(&service, workspace_id, hers, &sign_in).await,
        json!([])
    );
    server.shutdown().await;
}

/// Every file under `dir` called `name`.
fn files_called(dir: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                waiting.push(path);
            } else if path.file_name().is_some_and(|file| file == name) {
                found.push(path);
            }
        }
    }
    found
}

/// Copy the folder `from` to `to`, with everything under it.
fn copy_folder(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (path, copy) = (entry.path(), to.join(entry.file_name()));
        if path.is_dir() {
            copy_folder(&path, &copy);
        } else {
            std::fs::copy(&path, &copy).unwrap();
        }
    }
}

/// A server told where to keep the keys of people's sealed stores keeps
/// none of them in its state folder. So a copy of that folder, a backup or
/// a staging copy, opens nothing of a person's vault and makes no key for
/// it; with the keys beside it the same copy opens, which is all it lacked.
#[tokio::test]
async fn a_copy_of_the_state_folder_alone_opens_nothing_of_a_persons_vault() {
    let dir = tempfile::tempdir().unwrap();
    let (state, keys) = (dir.path().join("state"), dir.path().join("keys"));
    // The folder its operator made for the keys: a server makes none.
    std::fs::create_dir(&keys).unwrap();
    let (key, owner) = governor();
    let server = test_server_keeping_keys_apart(&state, &keys, &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let hers = (&ada, &caller(&ada));

    let login = a_login("https://example.test");
    let kept = ask(&actions, workspace_id, hers, person_vault::ADD, login)
        .await
        .expect("Ada keeps a login");
    let listed = ask(&actions, workspace_id, hers, person_vault::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(listed["items"], json!([kept]), "she reads it back");

    // What is sealed lies in the state folder; its key does not.
    assert_eq!(files_called(&state, "vault.enc").len(), 1);
    assert_eq!(
        files_called(&state, "vault.key"),
        Vec::<std::path::PathBuf>::new(),
        "the key of her vault lies in the state folder"
    );
    assert_eq!(files_called(&keys, "vault.key").len(), 1);
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());

    // A copy of the state folder alone.
    let copy = dir.path().join("copy");
    copy_folder(&state, &copy);
    let people = |state: &std::path::Path| state.join("workspaces").join("people");
    let hers_in = |state: &std::path::Path| people(state).join(hex::encode(ada.user_public_key));
    let opened = accounts::vault_items::VaultItems::open_in(&hers_in(&copy));
    assert!(
        opened.is_err(),
        "a copy of the state folder opened her vault"
    );
    assert_eq!(
        files_called(&copy, "vault.key"),
        Vec::<std::path::PathBuf>::new(),
        "and trying made no key for it"
    );

    // The same copy with the keys: the key was all it lacked.
    let copied_keys = dir.path().join("copied-keys");
    copy_folder(&keys, &copied_keys);
    accounts::keep_vault_keys_apart(&people(&copy), &copied_keys);
    let opened = accounts::vault_items::VaultItems::open_in(&hers_in(&copy))
        .expect("the copy opens with its keys");
    assert_eq!(opened.facts().len(), 1);
}

/// A server that kept each person's key in its state, beside the vault the
/// key opens, is then told a folder for the keys. Nobody is locked out of
/// their vault. The key moves under the folder the first time the vault is
/// opened, once it was read back from there: the very key, and no new one.
/// A key whose place under the folder cannot be made stays where it lies
/// and opens its vault from there, at that start and at the one after it.
#[cfg(unix)]
#[tokio::test]
async fn a_key_left_in_the_state_opens_its_vault_and_moves_apart_once_a_folder_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let asking = caller(&owner);
    let hers = (&owner, &asking);
    for movable in [true, false] {
        let state = dir.path().join(format!("state-{movable}"));
        let keys = dir.path().join(format!("keys-{movable}"));

        // Before any folder was named, her key lies in the state.
        let server = test_server_keeping_keys_apart(&state, no_folder(), &key, &owner).await;
        let (_, workspace_id, actions) = governed(server.workspaces()).await;
        let login = a_login("https://example.test");
        ask(&actions, workspace_id, hers, person_vault::ADD, login)
            .await
            .expect("she keeps a login");
        server.shutdown().await;
        let beside = files_called(&state, "vault.key");
        assert_eq!(beside.len(), 1, "her key lies beside her vault");
        let sealed_under = std::fs::read(&beside[0]).unwrap();

        // The folder is named, and it is there. Her keys' place under it is
        // free, or it is taken by a link that leads nowhere, under which
        // nothing can be made.
        std::fs::create_dir(&keys).unwrap();
        let her_place = keys.join(hex::encode(owner.user_public_key));
        if !movable {
            std::os::unix::fs::symlink(dir.path().join("nowhere"), &her_place).unwrap();
        }
        for (kept, start) in (1..).zip(["the first start with the folder", "the start after it"]) {
            let server = test_server_keeping_keys_apart(&state, &keys, &key, &owner).await;
            let (_, workspace_id, actions) = governed(server.workspaces()).await;
            let listed = ask(&actions, workspace_id, hers, person_vault::LIST, json!({}))
                .await
                .unwrap_or_else(|error| panic!("{start} locked her out of her vault: {error:?}"));
            assert_eq!(
                listed["items"].as_array().map(Vec::len),
                Some(kept),
                "{start}: {listed}"
            );
            let in_state = files_called(&state, "vault.key");
            let apart = files_called(&keys, "vault.key");
            if movable {
                assert_eq!(
                    in_state,
                    Vec::<std::path::PathBuf>::new(),
                    "{start}: her key still lies in the state"
                );
                assert_eq!(apart.len(), 1, "{start}: {apart:?}");
                assert!(apart[0].starts_with(&her_place), "{start}: {apart:?}");
                assert!(
                    std::fs::read(&apart[0]).unwrap() == sealed_under,
                    "{start}: the key kept apart is not the one her vault was sealed under"
                );
            } else {
                assert_eq!(in_state, beside, "{start}: her key left its place");
                assert_eq!(apart, Vec::<std::path::PathBuf>::new(), "{start}");
                assert!(
                    std::fs::read(&in_state[0]).unwrap() == sealed_under,
                    "{start}: her key was replaced"
                );
            }
            // What she keeps from now on is sealed under the same key.
            let more = a_login(&format!("https://{kept}.example.test"));
            ask(&actions, workspace_id, hers, person_vault::ADD, more)
                .await
                .unwrap_or_else(|error| panic!("{start} kept nothing more for her: {error:?}"));
            server.shutdown().await;
        }
    }
}

/// What a server is told whose operator named no folder for people's keys:
/// nothing, as an environment without the setting, or with it empty, reads.
fn no_folder() -> &'static std::path::Path {
    std::path::Path::new("")
}

/// What a server in a test wrote to its log.
#[derive(Clone, Default)]
struct Logged(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Logged {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logged {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

impl Logged {
    /// How much was written so far.
    fn written(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// The lines written after the first `from` bytes that say where
    /// people's keys are kept.
    fn said_since(&self, from: usize) -> Vec<String> {
        String::from_utf8_lossy(&self.0.lock().unwrap()[from..])
            .lines()
            .filter(|line| line.contains("the keys of people's sealed stores"))
            .map(str::to_owned)
            .collect()
    }
}

/// A server says as it starts where it keeps the keys of people's vaults:
/// one line at every start, a warning for as long as they lie in its state.
/// A folder it was named that is not there it does not make; it says so and
/// goes on opening each vault with the key beside it. Nothing it logs, at
/// any level, holds a key.
#[tokio::test]
async fn a_server_says_at_every_start_where_it_keeps_peoples_keys_and_logs_no_key() {
    let logged = Logged::default();
    let _subscribed = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(logged.clone())
            .finish(),
    );
    let dir = tempfile::tempdir().unwrap();
    let (state, keys) = (dir.path().join("state"), dir.path().join("keys"));
    let named = keys.display().to_string();
    let (key, owner) = governor();
    let asking = caller(&owner);
    let hers = (&owner, &asking);

    // No folder is named: the keys lie in the state, and the server says so.
    let from = logged.written();
    let server = test_server_keeping_keys_apart(&state, no_folder(), &key, &owner).await;
    let said = logged.said_since(from);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("WARN")
            && said[0].contains("are kept in the state folder")
            && said[0].contains("TILO_PEOPLE_KEYS names no folder"),
        "{said:?}"
    );
    let (_, workspace_id, actions) = governed(server.workspaces()).await;
    let login = a_login("https://example.test");
    ask(&actions, workspace_id, hers, person_vault::ADD, login)
        .await
        .expect("she keeps a login");
    server.shutdown().await;
    let beside = files_called(&state, "vault.key");
    assert_eq!(beside.len(), 1, "her key lies beside her vault");
    let her_key = std::fs::read(&beside[0]).unwrap();

    // A folder is named that is not there: the server makes none, says so,
    // and her vault opens with the key where it lies.
    let from = logged.written();
    let server = test_server_keeping_keys_apart(&state, &keys, &key, &owner).await;
    let said = logged.said_since(from);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("WARN")
            && said[0].contains("are kept in the state folder")
            && said[0].contains("where no folder is")
            && said[0].contains(&named),
        "{said:?}"
    );
    let (_, workspace_id, actions) = governed(server.workspaces()).await;
    let listed = ask(&actions, workspace_id, hers, person_vault::LIST, json!({}))
        .await
        .expect("her vault opens");
    assert_eq!(
        listed["items"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
    assert!(!keys.exists(), "the server made the folder it was named");
    assert_eq!(files_called(&state, "vault.key"), beside);
    server.shutdown().await;

    // The folder is there: the server says the keys are kept apart, and
    // warns of nothing.
    std::fs::create_dir(&keys).unwrap();
    let from = logged.written();
    let server = test_server_keeping_keys_apart(&state, &keys, &key, &owner).await;
    let said = logged.said_since(from);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("INFO")
            && said[0].contains("are kept apart from the state folder")
            && said[0].contains(&named),
        "{said:?}"
    );
    server.shutdown().await;

    let all = logged.0.lock().unwrap().clone();
    assert!(
        !all.windows(her_key.len()).any(|window| window == her_key),
        "the log holds her key as it is kept"
    );
    let text = String::from_utf8_lossy(&all);
    for spelled in [hex::encode(&her_key), hex::encode_upper(&her_key)] {
        assert!(!text.contains(&spelled), "the log holds her key in hex");
    }
}
