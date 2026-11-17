//! A person's vault on the host: what they keep to sign in with is added,
//! listed and removed only from their own device, a listing says only what
//! is safe to say, and no agent reaches any of it, whoever it acts for.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay handle a test binds is kept until the test ends; dropping it earlier closes the host it drives"
)]

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::remote::{person_accounts, person_models, person_vault};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::HostWorkspaceActions;
use super::super::tests::{admit, caller, governed, settled};
use crate::workspaces::tests::{governor, test_host};

const PASSWORD: &str = "correct horse battery staple";
const NAME: &str = "ada@example.test";
/// What no site shows as a setup key: it holds digits a seed never does.
const NOT_A_SEED: &str = "correct horse battery staple 0189";
const KEY: &str = "sk-live-ada-own-key";

/// `target` asked by `caller`, on the device `device` certifies, in the
/// workspace `workspace_id` carries.
pub(super) async fn ask_at(
    host: &HostWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    target: RemoteActionTarget,
    argument: Value,
) -> Result<Value, ActionHostError> {
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
        host.in_workspace_as(workspace_id, device)
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
    host: &HostWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    action: &str,
    argument: Value,
) -> Result<Value, ActionHostError> {
    ask_at(
        host,
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
    host: &HostWorkspaceActions,
    workspace_id: WorkspaceId,
    other: (&DeviceCert, &Caller),
    reference: &str,
) {
    let theirs = ask(host, workspace_id, other, person_vault::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(theirs["items"], json!([]), "{theirs}");
    let refused = ask(
        host,
        workspace_id,
        other,
        person_vault::REMOVE,
        json!({"item": reference}),
    )
    .await;
    assert!(
        matches!(&refused, Err(ActionHostError::Refused { reason }) if reason == "That item is not kept here"),
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

/// A person keeps a login from their own device. What the host answers,
/// then and in every listing, is its kind, where it belongs and when it was
/// added, under a reference: never the secret, never the name. Another
/// member, the workspace's owner included, lists nothing of it and removes
/// nothing of it, and the host's disk holds it nowhere in the clear.
#[tokio::test]
async fn a_person_keeps_what_they_sign_in_with_and_reads_back_only_what_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo, as_owner) = (caller(&ada), caller(&bo), caller(&owner));

    let kept = ask(
        &host,
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
        &host,
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
        reaches_nothing_of_it(&host, workspace_id, other, &reference).await;
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
        &host,
        workspace_id,
        (&ada, &as_ada),
        person_vault::REMOVE,
        json!({"item": reference}),
    )
    .await
    .expect("Ada forgets her login");
    assert_eq!(remaining["items"], json!([]));
    relay.shutdown().await;
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
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &host,
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
            "an agent the host admitted itself, acting for her",
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
        ] {
            let refused = ask(&host, workspace_id, (device, asking), action, argument).await;
            assert!(
                matches!(refused, Err(ActionHostError::UnknownAction)),
                "{who}, {action}: {refused:?}"
            );
        }
    }

    // Nothing of hers moved, and nothing was kept for anyone else.
    let hers = ask(
        &host,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(hers["items"], json!([kept]));
    let his = ask(
        &host,
        workspace_id,
        (&bo, &caller(&bo)),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(his["items"], json!([]));
    relay.shutdown().await;
}

/// An item that is not one to keep is refused before anything is kept, and
/// the refusal never repeats what was typed: a secret put where a place or a
/// kind belongs is not echoed back.
#[tokio::test]
async fn an_item_that_is_not_one_is_refused_without_repeating_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
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
            &host,
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
                ActionHostError::Refused { .. } | ActionHostError::InvalidRequest { .. }
            ),
            "{unusable}: {refused:?}"
        );
        let said = format!("{refused} {refused:?}");
        assert!(!said.contains(PASSWORD) && !said.contains("0189"), "{said}");
    }
    let refused = ask(
        &host,
        workspace_id,
        (&ada, &as_ada),
        person_vault::REMOVE,
        json!({"item": PASSWORD}),
    )
    .await
    .expect_err("what is no reference names nothing");
    assert!(!format!("{refused} {refused:?}").contains(PASSWORD));

    let hers = ask(
        &host,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(hers["items"], json!([]));
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    relay.shutdown().await;
}

/// A provider of whoever asks, at `https://models.example/v1`, by its id.
pub(super) async fn a_provider(
    host: &HostWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> String {
    let added = ask_at(
        host,
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
    host: &HostWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    (kind, place): (&str, &str),
) -> Value {
    let kept = ask(
        host,
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
    host: &HostWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    item: &Value,
) -> Value {
    let uses = ask(
        host,
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
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let provider = a_provider(&host, workspace_id, (&ada, &as_ada)).await;
    let item = kept_item(
        &host,
        workspace_id,
        (&ada, &as_ada),
        ("key", "https://models.example/v1/chat"),
    )
    .await;
    let helper = Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key)
        .named("Helper");

    let view = ask_at(
        &host,
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
    let sealed = accounts::AccountCredentials::open_in(&own)
        .unwrap()
        .get(&accounts::AccountId::from(format!("provider:{provider}")))
        .expect("the provider's key is kept");
    assert_eq!(sealed.password, KEY);
    assert!(files_holding(dir.path(), KEY.as_bytes()).is_empty());

    let listed = ask(
        &host,
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
    let uses = uses_of(&host, workspace_id, (&ada, &as_ada), &item).await;
    assert_eq!(uses.as_array().map(Vec::len), Some(1), "{uses}");
    assert_eq!(uses[0]["places"], json!(["https://models.example"]));
    assert_eq!(uses[0]["by"], "Helper");
    assert_eq!(fields(&uses[0]), ["at", "by", "places"]);
    let refused = ask(
        &host,
        workspace_id,
        (&ada, &helper),
        person_vault::USES,
        json!({"item": item}),
    )
    .await;
    assert!(
        matches!(refused, Err(ActionHostError::UnknownAction)),
        "{refused:?}"
    );
    relay.shutdown().await;
}

/// A provider of whoever asks and three items of theirs a use might name:
/// a key for another service, a login for the provider's site, and the
/// provider's own key.
async fn a_provider_and_items(
    host: &HostWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> (String, [Value; 3]) {
    let provider = a_provider(host, workspace_id, who).await;
    let elsewhere = kept_item(host, workspace_id, who, ("key", "https://other.example")).await;
    let login = kept_item(host, workspace_id, who, ("login", "https://models.example")).await;
    let right = kept_item(host, workspace_id, who, ("key", "https://models.example")).await;
    (provider, [elsewhere, login, right])
}

/// Nothing was kept and nothing used: the provider of `who` has no key and
/// none of `items` remembers a use.
async fn nothing_was_used(
    host: &HostWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    items: &[Value; 3],
) {
    let models = ask_at(
        host,
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
        assert_eq!(uses_of(host, workspace_id, who, item).await, json!([]));
    }
}

/// A reference is used only where its item belongs, as the kind the use
/// needs, for the person whose vault holds it: the wrong place, the wrong
/// kind and another person's vault are each refused, and nothing is used.
#[tokio::test]
async fn a_reference_is_refused_wherever_its_item_does_not_belong() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));
    let hers = (&ada, &as_ada);
    let (provider, items) = a_provider_and_items(&host, workspace_id, hers).await;
    let [elsewhere, login, right] = &items;
    let his_provider = a_provider(&host, workspace_id, (&bo, &as_bo)).await;
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
            &host,
            workspace_id,
            who,
            person_models::target(person_models::SET_KEY_FROM_ITEM),
            json!({"provider": provider, "key": item}),
        )
        .await;
        assert!(
            matches!(&refused, Err(ActionHostError::Refused { reason: said }) if said == reason),
            "{reason}: {refused:?}"
        );
    }
    nothing_was_used(&host, workspace_id, hers, &items).await;
    relay.shutdown().await;
}

/// A bot, an app and an agent no certificate stands behind use nothing of a
/// person's vault, and the person's own agent hands no typed key in: that
/// way stays the person's own.
#[tokio::test]
async fn only_a_persons_own_agent_uses_their_vault_and_only_by_reference() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let (provider, items) = a_provider_and_items(&host, workspace_id, hers).await;
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
            &host,
            workspace_id,
            (&ada, &other),
            target,
            json!({"provider": provider, "key": key}),
        )
        .await;
        assert!(
            matches!(refused, Err(ActionHostError::UnknownAction)),
            "{other:?}: {refused:?}"
        );
    }
    nothing_was_used(&host, workspace_id, hers, &items).await;
    relay.shutdown().await;
}

/// A mail sign-in named by reference goes only to the servers its item
/// belongs to: an account at any other server or port is refused before a
/// server is reached, and typing a password in stays the person's own.
#[tokio::test]
async fn a_mail_sign_in_by_reference_goes_only_to_its_own_servers() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let kept = ask(
        &host,
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
            &host,
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
            matches!(&refused, Err(ActionHostError::Refused { reason }) if reason == "That item belongs somewhere else"),
            "{incoming}:{port} {outgoing}: {refused:?}"
        );
    }
    let typed = ask_at(
        &host,
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
        matches!(typed, Err(ActionHostError::UnknownAction)),
        "{typed:?}"
    );

    let accounts = ask_at(
        &host,
        workspace_id,
        hers,
        person_accounts::target(person_accounts::LIST),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(accounts["accounts"], json!([]), "{accounts}");
    assert_eq!(
        uses_of(&host, workspace_id, hers, &sign_in).await,
        json!([])
    );
    relay.shutdown().await;
}
