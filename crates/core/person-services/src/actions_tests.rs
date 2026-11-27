//! What the person's own settings, providers and mail answer as actions of the workspace: the
//! paths of the vault, the model providers and the mail accounts under its root, the person's
//! model providers asked, their keys kept, and a host reaching only the network its operator
//! lets it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::{Map, Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER, workspace_governance_child,
};
use tilo_peer_state::join::Role;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use crate::fixtures::{
    admit, ask_own, caller, hosted, hosting_device, request, settled, test_host,
};
use tilo_workspace_service::workspaces::SetMemberRoleRequest;
use tilo_workspace_service::workspaces::actions::ServiceWorkspaceActions;

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn the_persons_vault_models_and_mail_are_paths_of_the_workspace() {
    use tilo_actions::remote::{WORKSPACE_MAIL_ACCOUNTS, WORKSPACE_MODELS, WORKSPACE_VAULT};
    use tilo_workspace_records::models as model_providers;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, host) = hosted(workspaces).await;
    let provider = host.device_page(workspace_id, &owner);
    let owner_caller = caller(&owner);
    let query = CatalogueQuery {
        provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
        parent_resource: Some(tilo_actions::remote::workspace_governance_resource(
            &workspace,
        )),
        max_results: 200,
        ..CatalogueQuery::default()
    };
    let page = settled(provider.query(&owner_caller, query)).await.unwrap();
    // Each of the person's own resources is published under the workspace's root, and the
    // action's address is the one the host dispatches to the provider it belongs to.
    let published = |child: &str, action: &str, params: usize| {
        let resource = workspace_governance_child(&workspace, Some(child));
        let row = page
            .rows
            .iter()
            .find(|row| {
                row.resource.id == resource
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|found| found.address.action == action)
            })
            .unwrap_or_else(|| panic!("{child}#{action} is published"));
        assert_eq!(row.provider, WORKSPACE_GOVERNANCE_PROVIDER);
        let found = row.action.as_ref().expect("a row of an action");
        assert_eq!(found.params.len(), params, "{child}#{action}");
        found.address.clone()
    };
    let listed = published(WORKSPACE_VAULT, tilo_workspace_records::vault::LIST, 0);
    assert_eq!(
        super::vault::vault_action(&listed),
        Some((workspace.as_str(), tilo_workspace_records::vault::LIST))
    );
    let add_provider = published(WORKSPACE_MODELS, model_providers::ADD_PROVIDER, 4);
    assert_eq!(
        super::models::models_action(&add_provider),
        Some((workspace.as_str(), model_providers::ADD_PROVIDER))
    );
    // The tree publishes adding an account under the verb `add`, which is its `tilo.accounts`
    // action `add-mail`.
    let add = published(
        WORKSPACE_MAIL_ACCOUNTS,
        "add",
        super::mail::add_account_params().len(),
    );
    assert_eq!(
        super::mail::account_action(&add),
        Some((
            workspace.as_str(),
            tilo_workspace_records::accounts::ADD_MAIL
        ))
    );
    // Asking one is handed to its provider, never refused as unknown here.
    for address in [listed, add_provider, add] {
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: address.clone(),
            },
            argument: InvokeArgument::Json(Map::new()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        if provider.invoke(&owner_caller, request).await
            == Some(Err(ActionCallError::UnknownAction))
        {
            panic!("{} is not dispatched to its provider", address.resource);
        }
    }
    server.shutdown().await;
}

/// Every file under `dir` whose bytes hold `needle`.
fn files_holding(dir: &std::path::Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                waiting.push(path);
            } else if metadata.is_file() {
                let bytes = std::fs::read(&path).unwrap();
                if bytes.windows(needle.len()).any(|w| w == needle) {
                    holding.push(path);
                }
            }
        }
    }
    holding
}

/// Every request of a person's model providers, the ones that name a
/// provider naming `provider`.
fn model_requests(provider: &str) -> Vec<(&'static str, Value)> {
    use tilo_workspace_records::models as model_providers;

    vec![
        (model_providers::LIST, json!({})),
        (
            model_providers::ADD_PROVIDER,
            json!({"name": "Another", "protocol": "open-ai-compatible", "endpoint": "https://other.example/v1"}),
        ),
        (model_providers::ADD_PRESET, json!({"preset": "anthropic"})),
        (
            model_providers::SET_KEY,
            json!({"provider": provider, "key": "sk-replaced"}),
        ),
        (model_providers::REMOVE_KEY, json!({"provider": provider})),
        (
            model_providers::REMOVE_PROVIDER,
            json!({"provider": provider}),
        ),
        (
            model_providers::REFRESH_MODELS,
            json!({"provider": provider}),
        ),
        (
            model_providers::ADD_MODEL,
            json!({"provider": provider, "model": "typed"}),
        ),
        (
            model_providers::SET_DEFAULT,
            json!({"provider": provider, "model": "typed"}),
        ),
        (
            model_providers::SET_ASSIGNMENT,
            json!({"kind": "task", "provider": provider, "model": "typed"}),
        ),
        (
            model_providers::SET_ASSIGNMENTS,
            json!({"kinds": ["request", "task"], "provider": provider, "model": "typed"}),
        ),
        (
            model_providers::SET_REASONING,
            json!({"provider": provider, "effort": "low"}),
        ),
    ]
}

/// What another workspace sees of the host's providers, and that naming
/// `provider` there reaches nothing of this workspace's.
async fn reaches_none_of_it(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    other: &DeviceCert,
    provider: &str,
) {
    use tilo_workspace_records::models as model_providers;

    let listed = ask_own(host, workspace_id, other, model_providers::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(listed["providers"], json!([]), "{listed}");
    // Every request that names a provider: the three that name none are the
    // other workspace's own to make, of its own store.
    let naming = model_requests(provider).into_iter().filter(|(verb, _)| {
        ![
            model_providers::LIST,
            model_providers::ADD_PROVIDER,
            model_providers::ADD_PRESET,
        ]
        .contains(verb)
    });
    for (verb, argument) in naming {
        let refused = ask_own(host, workspace_id, other, verb, argument).await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{verb}: {refused:?}"
        );
    }
}

/// A workspace's provider key is the workspace's, not a person's: the host
/// seals it in the workspace's own vault, every member reads back only that
/// one is kept, and the key is nowhere on the host's disk in the clear.
/// Another workspace on the same host neither lists the provider nor reaches
/// it through any request, nor finds its key in a store of its own.
#[tokio::test]
async fn a_workspaces_key_is_sealed_and_no_other_workspace_reaches_it() {
    use tilo_workspace_records::models as model_providers;
    const SECRET: &str = "sk-ada-own-secret";

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, host) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [31; 32],
                user_public_key: bo.user_public_key,
                role: Role::Moderator,
            },
        )
        .await
        .unwrap();

    let added = ask_own(
        &host,
        workspace_id,
        &ada,
        model_providers::ADD_PROVIDER,
        json!({"name": "Ada's provider", "protocol": "open-ai-compatible", "endpoint": "https://models.example/v1"}),
    )
    .await
    .expect("Ada adds a provider");
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    let keeping = json!({"provider": provider, "key": SECRET});
    ask_own(&host, workspace_id, &ada, model_providers::SET_KEY, keeping)
        .await
        .expect("Ada keeps her key");
    let hers = ask_own(&host, workspace_id, &ada, model_providers::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(hers["providers"][0]["configured"], json!(true), "{hers}");

    // Bo, another member of the workspace, reads the provider the workspace
    // keeps, and what he reads back says only that a key is kept.
    let his = ask_own(&host, workspace_id, &bo, model_providers::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(his["providers"][0]["configured"], json!(true), "{his}");
    assert!(!his.to_string().contains(SECRET), "{his}");

    // Another workspace on the same host reaches none of it.
    let (elsewhere, elsewhere_id, _) = hosted(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    reaches_none_of_it(&host, elsewhere_id, &cy, &provider).await;

    // The key is an entry of the workspace's own vault, the one its
    // provider names; the host keeps no vault of its own beside it, and the
    // key is nowhere on the host's disk in the clear.
    let own = workspaces.workspace_root(&workspace_id.to_string());
    let named = crate::person_settings::provider_config(&own)
        .profiles
        .iter()
        .find(|profile| profile.id == provider)
        .and_then(crate::person_settings::key_reference)
        .expect("the provider names its key");
    let ours = workspaces
        .open_vault(&workspace_id.to_string())
        .await
        .unwrap();
    assert_eq!(
        ours.entry(named.id()).map(|entry| entry.password.as_str()),
        Some(SECRET)
    );
    let theirs = workspaces
        .open_vault(&elsewhere_id.to_string())
        .await
        .unwrap();
    assert!(
        theirs.entry(named.id()).is_none(),
        "another workspace's vault holds this workspace's key"
    );
    assert!(!accounts::vault_dir_in(&own).exists());
    let clear = files_holding(dir.path(), SECRET.as_bytes());
    assert!(clear.is_empty(), "the key in the clear: {clear:?}");
}

/// An agent no device of a member's stands behind reaches nothing of the
/// workspace's model providers, whatever it asks: not one
/// the server admitted itself, not a bot and not an app. Unknown and
/// forbidden are the same answer, and nothing of the workspace's changed.
#[tokio::test]
async fn no_agent_with_no_device_behind_it_reaches_the_model_providers() {
    use agent_provider_config::protocol::{Message, ModelRequest, Role, ToolChoice};
    use tilo_workspace_records::models as model_providers;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, host) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let added = ask_own(
        &host,
        workspace_id,
        &ada,
        model_providers::ADD_PRESET,
        json!({"preset": "openrouter"}),
    )
    .await
    .expect("Ada adds a provider");
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    ask_own(
        &host,
        workspace_id,
        &ada,
        model_providers::SET_KEY,
        json!({"provider": provider, "key": "sk-ada-own-secret"}),
    )
    .await
    .expect("Ada keeps her key");
    let before = ask_own(&host, workspace_id, &ada, model_providers::LIST, json!({}))
        .await
        .unwrap();

    let person = hex::encode(ada.user_public_key);
    let agents = [
        Caller::local_agent("assistant", "ada"),
        Caller::local_bot("bot"),
        Caller::local_app("so.tilo.chat").acting_for(person),
    ];
    for agent in &agents {
        let mut asks = model_requests(&provider);
        asks.push((
            model_providers::ASK,
            serde_json::to_value(ModelRequest {
                messages: vec![Message::text(Role::User, "Spend the workspace's key")],
                tools: Vec::new(),
                tool_choice: ToolChoice::Auto,
                max_output_tokens: 16,
            })
            .unwrap(),
        ));
        for (verb, argument) in asks {
            let mut asking = request("", "", argument);
            asking.target = model_providers::target(verb);
            let refused = settled(host.device_page(workspace_id, &ada).invoke(agent, asking))
                .await
                .map(|_| ());
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{verb}: {refused:?}"
            );
        }
    }
    let after = ask_own(&host, workspace_id, &ada, model_providers::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(after["providers"][0]["configured"], json!(true), "{after}");
    server.shutdown().await;
}

/// The model of `workspace` is asked once, and it answers.
async fn asks_their_model(workspaces: &crate::PersonServices, workspace: WorkspaceId) {
    use agent_provider_config::Interaction;
    use agent_provider_config::protocol::{Message, ModelRequest, Role, ToolChoice};

    let request = ModelRequest {
        messages: vec![
            Message::text(Role::System, "You work for Ada."),
            Message::text(Role::User, "Is it time?"),
        ],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 256,
    };
    let deadline = tilo_clock::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap();
    workspaces
        .workspace_model(workspace, Interaction::Request)
        .answer(&request, deadline, &mut |_| {})
        .await
        .expect("the workspace's model answers");
}

/// What `at` was sent by the ask before.
fn sent(at: &test_http::Responder) -> Value {
    serde_json::from_slice(&at.next_request().body).unwrap()
}

/// One answer, as an OpenAI-compatible provider streams it.
const A_STREAMED_ANSWER: &str = concat!(
    "data: {\"id\":\"reply\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"large\",",
    "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Yes.\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"reply\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"large\",",
    "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"reply\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"large\",",
    "\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
    "data: [DONE]\n\n",
);

/// `person` asks for `effort` as the thinking level of their `provider`, or
/// with none takes their choice back.
async fn asks_for_level(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    provider: &str,
    effort: Option<&str>,
) -> Result<Value, ActionCallError> {
    let mut setting = json!({"provider": provider});
    if let Some(effort) = effort {
        setting["effort"] = json!(effort);
    }
    ask_own(
        actions,
        workspace_id,
        person,
        tilo_workspace_records::models::SET_REASONING,
        setting,
    )
    .await
}

/// The level the workspace's settings keep for `provider`, with the field
/// the provider is told it in.
fn kept_level(
    workspaces: &crate::PersonServices,
    workspace_id: WorkspaceId,
    provider: &str,
) -> Option<agent_provider_config::ReasoningSetting> {
    let root = workspaces.workspace_root(&workspace_id.to_string());
    crate::person_settings::provider_config(&root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .and_then(|profile| profile.reasoning)
}

/// The ready-made provider `name`, added for `person`: its id.
async fn ready_made(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    name: &str,
) -> String {
    ask_own(
        actions,
        workspace_id,
        person,
        tilo_workspace_records::models::ADD_PRESET,
        json!({"preset": name}),
    )
    .await
    .expect("a ready-made provider is added")["id"]
        .as_str()
        .expect("the provider's id")
        .to_owned()
}

/// How hard the workspace's provider thinks is its members' to choose, in one
/// request: the next ask of its model carries the level to the provider, a
/// word that is no level changes nothing, and with the level taken back the
/// provider is told nothing again, as before anybody chose. Each provider is
/// told in the field it takes a level in, and one that is asked for none
/// says so and keeps nothing.
#[tokio::test]
async fn a_thinking_level_reaches_the_workspaces_provider_with_the_next_ask() {
    use agent_provider_config::{ReasoningParam, ReasoningSetting};
    use test_http::{Reply, Responder};
    use tilo_workspace_records::models as model_providers;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, actions) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    // The provider is on this machine, which a server calls only with its
    // operator's leave.
    workspaces.call_private_model_addresses(true);
    let provider_at = Responder::handle(|request| {
        Some(if request.method == "GET" {
            Reply::status(200).body(r#"{"data":[{"id":"large"}]}"#)
        } else {
            Reply::status(200)
                .header("Content-Type", "text/event-stream")
                .body(A_STREAMED_ANSWER)
        })
    });
    let adding = json!({
        "name": "A provider",
        "protocol": "open-ai-compatible",
        "endpoint": format!("{}/v1", provider_at.url()),
    });
    let provider = links_a_provider(&actions, workspace_id, &ada, &adding, "sk-ada").await;
    let choosing = json!({"provider": provider, "model": "large"});
    ask_own(
        &actions,
        workspace_id,
        &ada,
        model_providers::SET_DEFAULT,
        choosing,
    )
    .await
    .expect("Ada chooses her model");
    provider_at.drain_requests();

    asks_their_model(workspaces, workspace_id).await;
    let before = sent(&provider_at);
    assert!(
        before.get("reasoning_effort").is_none() && before.get("reasoning").is_none(),
        "nothing is sent before anybody chose: {before}"
    );

    let chosen = asks_for_level(&actions, workspace_id, &ada, &provider, Some("low")).await;
    assert!(chosen.is_ok(), "a member's own choice is taken: {chosen:?}");
    assert_eq!(chosen.unwrap()["reasoning"], json!("low"));
    let listed = ask_own(
        &actions,
        workspace_id,
        &ada,
        model_providers::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        listed["providers"][0]["reasoning"],
        json!("low"),
        "{listed}"
    );
    asks_their_model(workspaces, workspace_id).await;
    let low = sent(&provider_at);
    assert_eq!(low["reasoning_effort"], json!("low"), "{low}");

    // A word that is no level is refused and changes nothing.
    let refused = asks_for_level(&actions, workspace_id, &ada, &provider, Some("lowest")).await;
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "{refused:?}"
    );
    asks_their_model(workspaces, workspace_id).await;
    let still = sent(&provider_at);
    assert_eq!(still["reasoning_effort"], json!("low"), "{still}");

    // Taken back: the provider is told nothing again.
    let cleared = asks_for_level(&actions, workspace_id, &ada, &provider, None).await;
    assert!(cleared.is_ok(), "the choice is taken back: {cleared:?}");
    assert!(cleared.unwrap().get("reasoning").is_none());
    asks_their_model(workspaces, workspace_id).await;
    let after = sent(&provider_at);
    assert!(
        after.get("reasoning_effort").is_none() && after.get("reasoning").is_none(),
        "{after}"
    );
    assert_eq!(kept_level(workspaces, workspace_id, &provider), None);

    // Each provider is told in the field it takes a level in.
    let router = ready_made(&actions, workspace_id, &ada, "openrouter").await;
    let taken = asks_for_level(&actions, workspace_id, &ada, &router, Some("high")).await;
    assert!(taken.is_ok(), "{taken:?}");
    assert_eq!(
        kept_level(workspaces, workspace_id, &router),
        Some(ReasoningSetting {
            param: ReasoningParam::ReasoningObject,
            effort: "high".into(),
        })
    );
    let anthropic = ready_made(&actions, workspace_id, &ada, "anthropic").await;
    let turned_away = asks_for_level(&actions, workspace_id, &ada, &anthropic, Some("high")).await;
    assert!(
        matches!(turned_away, Err(ActionCallError::Refused { .. })),
        "{turned_away:?}"
    );
    assert_eq!(kept_level(workspaces, workspace_id, &anthropic), None);
    server.shutdown().await;
}

/// A provider `person` links as `adding` says, its `key` kept and its models
/// asked for: the provider's id.
async fn links_a_provider(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    adding: &Value,
    key: &str,
) -> String {
    use tilo_workspace_records::models as model_providers;

    let added = ask_own(
        host,
        workspace_id,
        person,
        model_providers::ADD_PROVIDER,
        adding.clone(),
    )
    .await
    .expect("a provider is added");
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    ask_own(
        host,
        workspace_id,
        person,
        model_providers::SET_KEY,
        json!({"provider": provider, "key": key}),
    )
    .await
    .expect("the key is kept");
    let listed = ask_own(
        host,
        workspace_id,
        person,
        model_providers::REFRESH_MODELS,
        json!({"provider": provider}),
    )
    .await
    .expect("the provider is asked");
    assert!(listed.get("problem").is_none(), "{listed}");
    assert!(!listed.to_string().contains(key), "{listed}");
    provider
}

/// A provider on this machine that lists `sk-ada`'s models to that key and
/// `sk-bo`'s to that one, and where a page adds it.
fn provider_of_two_keys() -> (test_http::Responder, Value) {
    use test_http::{Reply, Responder};

    let fake = Responder::handle(|request| {
        let models = match request.header("authorization") {
            Some("Bearer sk-ada") => r#"{"data":[{"id":"ada-large"},{"id":"ada-small"}]}"#,
            Some("Bearer sk-bo") => r#"{"data":[{"id":"bo-only"}]}"#,
            _ => return Some(Reply::status(401)),
        };
        Some(Reply::status(200).body(models))
    });
    let adding = json!({
        "name": "Fake provider",
        "protocol": "open-ai-compatible",
        "endpoint": format!("{}/v1", fake.url()),
    });
    (fake, adding)
}

/// The model `kind` of work runs on in the listing `reads`.
fn model_for(reads: &Value, kind: &str) -> Option<Value> {
    reads["assignments"]
        .as_array()
        .and_then(|chosen| chosen.iter().find(|chosen| chosen["kind"] == kind))
        .map(|chosen| chosen["model"].clone())
}

/// Two workspaces on one host each link a provider with a key of their own.
/// The host asks the provider for its models with each workspace's key, each
/// chooses the models its kinds of work run on, and neither reads the
/// other's or chooses from them.
#[tokio::test]
async fn each_workspace_links_a_provider_and_chooses_its_own_models() {
    use tilo_workspace_records::models as model_providers;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, host) = hosted(workspaces).await;
    let (elsewhere, elsewhere_id, _) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &elsewhere, &owner, 2).await;
    // The provider is on this machine, which a server calls only with its
    // operator's leave.
    workspaces.call_private_model_addresses(true);
    let (_fake, adding) = provider_of_two_keys();
    let hers = links_a_provider(&host, workspace_id, &ada, &adding, "sk-ada").await;
    let his = links_a_provider(&host, elsewhere_id, &bo, &adding, "sk-bo").await;

    for (here, person, verb, choosing) in [
        (
            workspace_id,
            &ada,
            model_providers::SET_DEFAULT,
            json!({"provider": hers, "model": "ada-small"}),
        ),
        (
            workspace_id,
            &ada,
            model_providers::SET_ASSIGNMENTS,
            json!({"kinds": ["request", "task"], "provider": hers, "model": "ada-large"}),
        ),
        (
            workspace_id,
            &ada,
            model_providers::SET_ASSIGNMENT,
            json!({"kind": "subtask", "provider": hers, "model": "ada-small"}),
        ),
        (
            elsewhere_id,
            &bo,
            model_providers::SET_DEFAULT,
            json!({"provider": his, "model": "bo-only"}),
        ),
    ] {
        ask_own(&host, here, person, verb, choosing)
            .await
            .expect("a model is chosen");
    }

    let ada_reads = ask_own(&host, workspace_id, &ada, model_providers::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(ada_reads["defaultProvider"], json!(hers), "{ada_reads}");
    assert_eq!(
        ada_reads["providers"][0]["defaultModel"],
        json!("ada-small")
    );
    let offered: Vec<&str> = ada_reads["providers"][0]["models"]
        .as_array()
        .expect("the models the provider listed")
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect();
    assert_eq!(offered, ["ada-large", "ada-small"]);
    assert_eq!(model_for(&ada_reads, "request"), Some(json!("ada-large")));
    assert_eq!(model_for(&ada_reads, "task"), Some(json!("ada-large")));
    assert_eq!(model_for(&ada_reads, "subtask"), Some(json!("ada-small")));
    assert_eq!(model_for(&ada_reads, "classifier"), None);

    let bo_reads = ask_own(&host, elsewhere_id, &bo, model_providers::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(bo_reads["defaultProvider"], json!(his), "{bo_reads}");
    assert_eq!(bo_reads["providers"].as_array().map(Vec::len), Some(1));
    assert_eq!(bo_reads["providers"][0]["defaultModel"], json!("bo-only"));
    assert!(bo_reads.get("assignments").is_none(), "{bo_reads}");
    // His own provider offers no model of hers, and hers is no provider of
    // his.
    for provider in [&his, &hers] {
        let refused = ask_own(
            &host,
            elsewhere_id,
            &bo,
            model_providers::SET_DEFAULT,
            json!({"provider": provider, "model": "ada-large"}),
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{refused:?}"
        );
    }
    server.shutdown().await;
}

/// A host calls a provider on its own network only with its operator's
/// leave: without it no provider is kept there, and one kept while it had
/// the leave is no longer asked.
#[tokio::test]
async fn a_host_reaches_its_own_network_only_with_its_operators_leave() {
    use tilo_workspace_records::models as model_providers;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, host) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let (fake, adding) = provider_of_two_keys();

    let unkept = ask_own(
        &host,
        workspace_id,
        &ada,
        model_providers::ADD_PROVIDER,
        adding.clone(),
    )
    .await;
    assert!(
        matches!(unkept, Err(ActionCallError::Refused { .. })),
        "{unkept:?}"
    );
    assert!(fake.drain_requests().is_empty());

    workspaces.call_private_model_addresses(true);
    let hers = links_a_provider(&host, workspace_id, &ada, &adding, "sk-ada").await;
    // Once to ask whether it takes the key before it is kept, once to list
    // its models.
    assert_eq!(fake.drain_requests().len(), 2);

    workspaces.call_private_model_addresses(false);
    let uncalled = ask_own(
        &host,
        workspace_id,
        &ada,
        model_providers::REFRESH_MODELS,
        json!({"provider": hers}),
    )
    .await
    .expect("an answer that says why not");
    assert_eq!(uncalled["problem"], json!("not-allowed"), "{uncalled}");
    assert!(fake.drain_requests().is_empty());
    server.shutdown().await;
}

/// The person finds their workspace's model providers on their own device, by the address their
/// windows and palette call (`tilo.models/models#list`), and what they list answers: every action
/// the providers declare but a turn of the model. An agent or an app acting for them is offered
/// none of it.
#[tokio::test]
async fn the_person_finds_their_model_providers_by_their_address() {
    use tilo_workspace_records::models as model_providers;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (_workspace, workspace_id, host) = hosted(workspaces).await;
    let page = host.device_page(workspace_id, &owner);
    let query = CatalogueQuery {
        provider: Some(model_providers::PROVIDER.into()),
        max_results: 200,
        ..CatalogueQuery::default()
    };
    let listed = |rows: &[tilo_actions::remote::CatalogueRow]| -> Vec<String> {
        rows.iter()
            .filter(|row| row.provider == model_providers::PROVIDER)
            .filter_map(|row| row.action.as_ref())
            .filter(|action| action.address.resource == model_providers::RESOURCE)
            .map(|action| action.address.action.clone())
            .collect()
    };
    let offered = listed(
        &settled(page.query(&caller(&owner), query.clone()))
            .await
            .unwrap()
            .rows,
    );
    for action in [
        model_providers::LIST,
        model_providers::ADD_PROVIDER,
        model_providers::SET_KEY,
        model_providers::SET_DEFAULT,
    ] {
        assert!(
            offered.iter().any(|found| found == action),
            "{action}: {offered:?}"
        );
    }
    assert!(
        !offered.iter().any(|found| found == model_providers::ASK),
        "{offered:?}"
    );
    let answered = ask_own(
        &host,
        workspace_id,
        &owner,
        model_providers::LIST,
        json!({}),
    )
    .await
    .expect("the listed read answers");
    assert!(answered["providers"].is_array(), "{answered}");

    let person = hex::encode(owner.user_public_key);
    for acting in [
        Caller::local_agent("assistant", "owner"),
        Caller::local_app("so.tilo.chat").acting_for(person),
    ] {
        let rows = settled(page.query(&acting, query.clone()))
            .await
            .map(|page| page.rows)
            .unwrap_or_default();
        assert!(listed(&rows).is_empty(), "{:?}", listed(&rows));
    }
    server.shutdown().await;
}
