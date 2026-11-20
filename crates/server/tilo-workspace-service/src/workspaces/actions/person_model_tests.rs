//! A person's model from the store's own actions to the answer: what their
//! own device keeps through Settings' actions is what answers work done for
//! them, no agent reads or changes any of it, and no log line carries a key.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives"
)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use agent_provider_config::protocol::{Content, Message, ModelRequest, Role, ToolChoice};
use serde_json::{Value, json};
use test_http::Responder;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, ReceiptStatus, RemoteActionTarget, person_models,
    person_vault,
};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::ServiceWorkspaceActions;
use super::tests::{admit, ask_own, governed, request, settled};
use crate::workspaces::Workspaces;
use crate::workspaces::person_model::scripted::{Reply, ScriptedProvider};
use crate::workspaces::person_model::{Interaction, ModelRefusal};
use crate::workspaces::tests::{governor, test_host};

const ADA_KEY: &str = "sk-ada-linked-through-settings";
const BO_KEY: &str = "sk-bo-linked-through-settings";

fn asking(text: &str) -> ModelRequest {
    ModelRequest {
        messages: vec![Message::text(Role::User, text)],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 256,
    }
}

fn soon() -> tilo_clock::time::Instant {
    tilo_clock::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap()
}

/// The lines a subscriber wrote.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

impl Lines {
    /// Everything this thread logs while the answer is held is written here.
    fn listening(&self) -> tracing::subscriber::DefaultGuard {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(self.clone())
            .finish();
        tracing::subscriber::set_default(subscriber)
    }

    fn written(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// A server keeping its state under `dir`, with one workspace, the
/// workspace's actions and the device of one person in it. The first value
/// keeps the server up for as long as it is held.
async fn served(
    dir: &Path,
) -> (
    impl Sized,
    Workspaces,
    WorkspaceId,
    ServiceWorkspaceActions,
    DeviceCert,
) {
    let (key, owner) = governor();
    let server = test_host(dir, &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let (workspace, workspace_id, service) = governed(&workspaces).await;
    let ada = admit(&workspaces, &workspace, &owner, 1).await;
    (server, workspaces, workspace_id, service, ada)
}

/// The one model the stand-in provider lists.
const LISTED: &str = "stand-in-model";
/// A key the stand-in provider gives no answer to.
const UNANSWERED_KEY: &str = "sk-made-up-and-given-no-answer";

/// A provider on this machine that lists its one model to whoever shows a
/// key of `taken`, gives [`UNANSWERED_KEY`] no answer at all and turns every
/// other key away: the provider, and the endpoint a person adds it at.
fn stand_in(taken: &'static [&'static str]) -> (Responder, String) {
    let provider = Responder::handle(move |request| {
        let shown = request
            .header("authorization")
            .unwrap_or_default()
            .trim_start_matches("Bearer ");
        if shown == UNANSWERED_KEY {
            return None;
        }
        if !taken.contains(&shown) {
            return Some(test_http::Reply::status(401).body(r#"{"error":"that key is unknown"}"#));
        }
        Some(
            test_http::Reply::status(200)
                .header("Content-Type", "application/json")
                .body(format!(r#"{{"data":[{{"id":"{LISTED}"}}]}}"#)),
        )
    });
    let endpoint = format!("{}/v1", provider.url());
    (provider, endpoint)
}

/// One request of `device`'s own person at `target`.
async fn ask_at(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    device: &DeviceCert,
    target: RemoteActionTarget,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let mut asking = request("", "", argument);
    asking.target = target;
    let person = Caller::authenticated(device.user_public_key, device.device_public_key);
    let receipt = settled(
        service
            .in_workspace_as(workspace_id, device)
            .invoke(&person, asking),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a person's own store answers at once: {other:?}"),
    }
}

/// The key `person`'s model is asked with now: one call of their own model,
/// which `scripted` answers.
async fn asked_with(
    workspaces: &Workspaces,
    person: &DeviceCert,
    scripted: &ScriptedProvider,
) -> Option<String> {
    scripted.then(Reply::text("an answer"));
    workspaces
        .person_model(person.user_public_key, Interaction::Request)
        .answer(&asking("Hello?"), soon(), &mut |_| {})
        .await
        .expect("the person's model answers");
    scripted.asked().last().and_then(|asked| asked.key.clone())
}

/// A person at a server that reaches the stand-in provider, their provider
/// linked there with `kept` and its one model chosen: the provider's id.
async fn linked_at_stand_in(
    workspaces: &Workspaces,
    (service, workspace_id, person): (&ServiceWorkspaceActions, WorkspaceId, &DeviceCert),
    (endpoint, kept): (&str, &str),
) -> String {
    workspaces.call_private_model_addresses(true);
    let provider = link(service, workspace_id, person, ("Stand-in", endpoint, kept)).await;
    let listed = ask_own(
        service,
        workspace_id,
        person,
        person_models::REFRESH_MODELS,
        json!({"provider": provider}),
    )
    .await
    .expect("the provider is asked for its models");
    assert!(listed.get("problem").is_none(), "{listed}");
    ask_own(
        service,
        workspace_id,
        person,
        person_models::SET_DEFAULT,
        json!({"provider": provider, "model": LISTED}),
    )
    .await
    .expect("the provider's model is chosen");
    provider
}

/// `device`'s person adds a provider and keeps its key, through the actions
/// Settings asks with: the provider's id.
async fn link(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    device: &DeviceCert,
    (name, endpoint, key): (&str, &str, &str),
) -> String {
    let added = ask_own(
        host,
        workspace_id,
        device,
        person_models::ADD_PROVIDER,
        json!({"name": name, "protocol": "open-ai-compatible", "endpoint": endpoint}),
    )
    .await
    .expect("the person adds a provider");
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    ask_own(
        host,
        workspace_id,
        device,
        person_models::SET_KEY,
        json!({"provider": provider, "key": key}),
    )
    .await
    .expect("the person keeps its key");
    provider
}

/// One of the model actions asked by `caller` on `device`'s session.
async fn ask_as(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    device: &DeviceCert,
    caller: &Caller,
    (verb, argument): (&str, Value),
) -> Result<Value, ActionCallError> {
    let mut request = request("", "", argument);
    request.target = person_models::target(verb);
    let receipt = settled(
        host.in_workspace_as(workspace_id, device)
            .invoke(caller, request),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a person's own settings answer at once: {other:?}"),
    }
}

/// Two people on one host each link a provider and choose a model for a
/// kind of work, as their own devices, through the actions Settings asks
/// with. Work done for each is answered by their own model, asked with
/// their own key; a kind neither chose a model for is refused.
#[tokio::test]
async fn two_people_on_one_host_are_each_answered_by_their_own_model() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let scripted =
        ScriptedProvider::answering([Reply::text("for Ada").costing(700), Reply::text("for Bo")]);
    workspaces.answer_models_with(scripted.clone());

    for (person, linking, model, kind) in [
        (
            &ada,
            (
                "Google",
                "https://generativelanguage.googleapis.com/v1beta/openai",
                ADA_KEY,
            ),
            "gemini-main",
            "request",
        ),
        (
            &bo,
            ("Bo's own", "https://models.example.com/v1", BO_KEY),
            "open-model",
            "request",
        ),
    ] {
        let provider = link(&host, workspace_id, person, linking).await;
        // The provider's own list of models, which the host reads with the
        // key once that lands; until then a provider added from a page
        // offers none, so the list is written here.
        workspaces
            .offer_models_for(&person.user_public_key, &provider, &[model])
            .unwrap();
        let chosen = ask_own(
            &host,
            workspace_id,
            person,
            person_models::SET_ASSIGNMENT,
            json!({"kind": kind, "provider": provider, "model": model}),
        )
        .await
        .expect("the person chooses a model for a kind of work");
        assert_eq!(chosen["assignments"][0]["model"], json!(model), "{chosen}");
    }

    let hers = workspaces
        .person_model(ada.user_public_key, Interaction::Request)
        .answer(&asking("Good morning"), soon(), &mut |_| {})
        .await
        .unwrap();
    let his = workspaces
        .person_model(bo.user_public_key, Interaction::Request)
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(hers.linked.provider, "Google");
    assert_eq!(hers.response.cost_micros, 700);
    assert_eq!(his.linked.provider, "Bo's own");
    assert!(
        matches!(his.response.message.content.as_slice(), [Content::Text { text }] if text == "for Bo")
    );
    let asked = scripted.asked();
    assert_eq!(
        asked
            .iter()
            .map(|asked| (asked.model.as_str(), asked.key.as_deref()))
            .collect::<Vec<_>>(),
        vec![("gemini-main", Some(ADA_KEY)), ("open-model", Some(BO_KEY))]
    );

    // Neither chose a model for judging, and neither has a default: the
    // other's model never answers in its place.
    let refused = workspaces
        .person_model(ada.user_public_key, Interaction::Classifier)
        .answer(&asking("Is this one?"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::NoModelLinked {
            kind: Interaction::Classifier
        }
    );
    assert_eq!(scripted.asked().len(), 2);
}

/// No agent reaches a person's providers, whichever way it is admitted and
/// also when it acts for that very person: it reads no listing, and it
/// neither replaces nor removes a key. What the person themselves reads back
/// says only that a key is kept.
#[tokio::test]
async fn no_agent_reads_or_changes_a_persons_key() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let provider = link(
        &host,
        workspace_id,
        &ada,
        ("Google", "https://models.example.com/v1", ADA_KEY),
    )
    .await;
    workspaces
        .offer_models_for(&ada.user_public_key, &provider, &["gemini-main"])
        .unwrap();

    let person = hex::encode(ada.user_public_key);
    let agents = [
        Caller::delegated_agent("helper", ada.user_public_key, ada.device_public_key),
        Caller::bot("installed", ada.user_public_key, ada.device_public_key),
        Caller::agent_turn(
            "helper",
            person.clone(),
            tilo_action_policy::turn::Turn::new("chat/main", "message-1"),
        ),
        Caller::local_agent("helper"),
        Caller::authenticated(ada.user_public_key, ada.device_public_key).delegated_to("helper"),
    ];
    for agent in &agents {
        for asking in [
            (person_models::LIST, json!({})),
            (
                person_models::SET_KEY,
                json!({"provider": provider, "key": "sk-an-agents-own"}),
            ),
            (person_models::REMOVE_KEY, json!({"provider": provider})),
            (
                person_models::SET_ASSIGNMENT,
                json!({"kind": "task", "provider": provider, "model": "gemini-main"}),
            ),
            (
                person_models::REMOVE_PROVIDER,
                json!({"provider": provider}),
            ),
            (
                person_models::ASK,
                serde_json::to_value(asking("Spend her key")).unwrap(),
            ),
        ] {
            let verb = asking.0;
            // The agent her own computer hands its session to asks her model as that computer
            // does ([`the_person_and_their_own_computers_agent_ask_their_model`]); her keys
            // are no agent's either way.
            if verb == person_models::ASK && agent.is_session_delegated() {
                continue;
            }
            let refused = ask_as(&host, workspace_id, &ada, agent, asking).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{verb} as {:?}: {refused:?}",
                agent.acting()
            );
        }
    }

    // Her own reading says a key is kept and carries none.
    let hers = ask_own(&host, workspace_id, &ada, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(hers["providers"][0]["configured"], json!(true), "{hers}");
    assert_eq!(hers["assignments"], Value::Null, "no agent chose a model");
    assert!(!hers.to_string().contains(ADA_KEY), "{hers}");

    // And her key is still the one her model is asked with.
    workspaces
        .choose_model_for(
            &ada.user_public_key,
            Interaction::Task,
            &provider,
            "gemini-main",
        )
        .unwrap();
    let scripted = ScriptedProvider::answering([Reply::text("done")]);
    workspaces.answer_models_with(scripted.clone());
    workspaces
        .person_model(ada.user_public_key, Interaction::Task)
        .answer(&asking("Do it"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(scripted.asked()[0].key.as_deref(), Some(ADA_KEY));
}

/// A page's window asks the person's model through their server: the turn is
/// answered by the model they set up in Settings, with the key their vault
/// keeps, and a person who set none up is told so, in a failure whose kind
/// the window decides by.
#[tokio::test]
async fn a_page_asks_the_persons_model_through_their_host() {
    use agent_provider_config::protocol::{ErrorKind, ModelError, ModelResponse};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let scripted = ScriptedProvider::answering([Reply::text("from the server")]);
    workspaces.answer_models_with(scripted.clone());

    let provider = link(
        &host,
        workspace_id,
        &ada,
        ("Google", "https://models.example.com/v1", ADA_KEY),
    )
    .await;
    workspaces
        .offer_models_for(&ada.user_public_key, &provider, &["gemini-main"])
        .unwrap();
    ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::SET_DEFAULT,
        json!({"provider": provider, "model": "gemini-main"}),
    )
    .await
    .unwrap();

    let turn = serde_json::to_value(asking("Summarise my day")).unwrap();
    let hers = ask_own(&host, workspace_id, &ada, person_models::ASK, turn.clone())
        .await
        .unwrap();
    let hers: Result<ModelResponse, ModelError> = serde_json::from_value(hers).unwrap();
    let hers = hers.unwrap();
    assert!(
        matches!(hers.message.content.as_slice(), [Content::Text { text }] if text == "from the server")
    );
    let asked = scripted.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].model, "gemini-main");
    assert_eq!(asked[0].key.as_deref(), Some(ADA_KEY));

    let his = ask_own(&host, workspace_id, &bo, person_models::ASK, turn)
        .await
        .unwrap();
    let his: Result<ModelResponse, ModelError> = serde_json::from_value(his).unwrap();
    assert_eq!(his.unwrap_err().kind, ErrorKind::NoProvider);
    assert_eq!(scripted.asked().len(), 1, "nobody else's model answers him");
}

/// A page's ask is answered as the model writes it: the server admits it at
/// once, its items each hold the answer so far, every one a beginning of the
/// next, and its output is the whole turn.
#[tokio::test]
async fn a_pages_ask_is_answered_as_the_model_writes_it() {
    use agent_provider_config::protocol::{ModelError, ModelResponse};
    use tilo_actions::remote::{ActionProvider as _, ReceiptStatus, UpdateMark, settle_with};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions_here) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let said = "one two three four five six seven eight";
    workspaces.answer_models_with(ScriptedProvider::answering([Reply::text(said)]));
    let provider = link(
        &actions_here,
        workspace_id,
        &ada,
        ("Google", "https://models.example.com/v1", ADA_KEY),
    )
    .await;
    workspaces
        .offer_models_for(&ada.user_public_key, &provider, &["gemini-main"])
        .unwrap();
    ask_own(
        &actions_here,
        workspace_id,
        &ada,
        person_models::SET_DEFAULT,
        json!({"provider": provider, "model": "gemini-main"}),
    )
    .await
    .unwrap();

    let actions = actions_here.in_workspace_as(workspace_id, &ada);
    let her = super::tests::caller(&ada);
    let mut request = super::tests::request(
        "",
        "",
        serde_json::to_value(asking("Count to eight")).unwrap(),
    );
    request.target = person_models::target(person_models::ASK);
    let invocation = request.invocation;
    let receipt = super::tests::settled(actions.invoke(&her, request))
        .await
        .unwrap();
    assert!(
        matches!(receipt.status, ReceiptStatus::Accepted),
        "an ask is admitted at once: {receipt:?}"
    );
    let now = tilo_clock::now();
    let until = now.checked_add(tilo_test_env::hang_bound()).unwrap_or(now);
    let mut seen = UpdateMark::Accepted;
    let mut written = String::new();
    let output = loop {
        let state =
            super::tests::settled(settle_with(&actions, &her, invocation, Some(seen), until))
                .await
                .expect("her ask is hers to follow");
        seen = UpdateMark::of(&state.update);
        if let tilo_actions::InvocationUpdate::Item { value, .. } = &state.update {
            let so_far = value["text"].as_str().unwrap().to_owned();
            assert!(so_far.starts_with(&written), "{so_far:?} after {written:?}");
            assert!(said.starts_with(&so_far), "{so_far:?}");
            written = so_far;
        }
        match state.receipt().status {
            ReceiptStatus::Accepted => {}
            ReceiptStatus::Completed { output } => break output,
            other => panic!("{other:?}"),
        }
    };
    let answer: Result<ModelResponse, ModelError> =
        serde_json::from_value(output.value.unwrap()).unwrap();
    assert!(
        matches!(answer.unwrap().message.content.as_slice(), [Content::Text { text }] if text == said)
    );
}

/// Everything written to the log while a key is kept, a model is asked and
/// a call fails holds no key.
#[tokio::test]
async fn no_log_line_carries_a_key() {
    let lines = Lines::default();
    let _logged = lines.listening();

    let dir = tempfile::tempdir().unwrap();
    let (_server, workspaces, workspace_id, host, ada) = served(dir.path()).await;
    // A provider that takes both keys: a kept key gives way only to one its
    // provider takes.
    workspaces.call_private_model_addresses(true);
    let (_stand_in, endpoint) = stand_in(&[ADA_KEY, BO_KEY]);
    let provider = link(&host, workspace_id, &ada, ("Google", &endpoint, ADA_KEY)).await;
    workspaces
        .offer_models_for(&ada.user_public_key, &provider, &["gemini-main"])
        .unwrap();
    workspaces
        .choose_model_for(
            &ada.user_public_key,
            Interaction::Request,
            &provider,
            "gemini-main",
        )
        .unwrap();
    workspaces.answer_models_with(ScriptedProvider::answering([
        Reply::text("Hello"),
        Reply::failing(agent_provider_config::protocol::ModelError::new(
            agent_provider_config::protocol::ErrorKind::Authentication,
        )),
    ]));
    let model = workspaces.person_model(ada.user_public_key, Interaction::Request);
    model
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap();
    let failed = model.answer(&asking("Again"), soon(), &mut |_| {}).await;
    tracing::warn!(refused = ?failed, "a person's model did not answer");
    // A key replaced and removed is logged no more than one kept.
    ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::SET_KEY,
        json!({"provider": provider, "key": BO_KEY}),
    )
    .await
    .unwrap();
    ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::REMOVE_KEY,
        json!({"provider": provider}),
    )
    .await
    .unwrap();

    let written = lines.written();
    assert!(
        written.contains("a person's model did not answer"),
        "the log was read: {written}"
    );
    for secret in [ADA_KEY, BO_KEY] {
        assert!(!written.contains(secret), "a key was logged: {written}");
    }
}

/// None of `keys` stands in an answer of `said` or in what was `written` to
/// the log.
fn carried_nowhere(keys: &[&str], said: &[String], written: &str) {
    for key in keys {
        assert!(
            !said.iter().any(|answer| answer.contains(key)),
            "an answer carries a key: {said:?}"
        );
        assert!(!written.contains(key), "a key was logged: {written}");
    }
}

/// A person whose provider keeps a key types another one. A key the
/// provider turns away, a key no request can carry and a key the provider
/// gives no answer about are not kept: the key kept before stays the one
/// their model and their provider's list are asked with, and they are told
/// so. No answer and no log line carries any of the keys.
#[tokio::test]
async fn a_key_the_provider_does_not_take_leaves_the_kept_key_in_use() {
    const KEPT_KEY: &str = "sk-made-up-and-kept-first";
    const UNKNOWN_KEY: &str = "sk-made-up-and-unknown-there";
    const UNCARRIED_KEY: &str = "sk-made-up\u{1}and-carried-by-no-request";

    let lines = Lines::default();
    let _logged = lines.listening();
    let dir = tempfile::tempdir().unwrap();
    let (_server, workspaces, workspace_id, service, ada) = served(dir.path()).await;
    let scripted = ScriptedProvider::answering([]);
    workspaces.answer_models_with(scripted.clone());
    let (stand_in, endpoint) = stand_in(&[KEPT_KEY]);
    let provider = linked_at_stand_in(
        &workspaces,
        (&service, workspace_id, &ada),
        (&endpoint, KEPT_KEY),
    )
    .await;
    assert_eq!(
        asked_with(&workspaces, &ada, &scripted).await.as_deref(),
        Some(KEPT_KEY)
    );
    let mut said = Vec::new();

    for (typed, which, why) in [
        (
            UNKNOWN_KEY,
            "a key the provider turns away",
            "did not accept this key",
        ),
        (
            UNCARRIED_KEY,
            "a key no request can carry",
            "did not accept this key",
        ),
        (
            UNANSWERED_KEY,
            "a key the provider gives no answer about",
            "could not be asked about this key now",
        ),
    ] {
        let answered = ask_own(
            &service,
            workspace_id,
            &ada,
            person_models::SET_KEY,
            json!({"provider": provider, "key": typed}),
        )
        .await;
        let Err(ActionCallError::Refused { reason }) = &answered else {
            panic!("{which} was kept: {answered:?}");
        };
        assert_eq!(
            reason,
            &format!("Stand-in {why}. The key kept before is still in use."),
            "{which}"
        );
        said.push(reason.clone());
        let hers = ask_own(&service, workspace_id, &ada, person_models::LIST, json!({}))
            .await
            .unwrap();
        assert_eq!(hers["providers"][0]["configured"], json!(true), "{hers}");
        said.push(hers.to_string());
        assert_eq!(
            asked_with(&workspaces, &ada, &scripted).await.as_deref(),
            Some(KEPT_KEY),
            "after {which}"
        );
    }

    // The provider's own list is asked with the kept key still.
    stand_in.drain_requests();
    let listed = ask_own(
        &service,
        workspace_id,
        &ada,
        person_models::REFRESH_MODELS,
        json!({"provider": provider}),
    )
    .await
    .expect("the provider is asked for its models");
    assert!(listed.get("problem").is_none(), "{listed}");
    said.push(listed.to_string());
    let shown = stand_in.drain_requests();
    assert_eq!(shown.len(), 1, "the list is asked for once");
    assert_eq!(
        shown[0].header("authorization"),
        Some(format!("Bearer {KEPT_KEY}").as_str())
    );
    carried_nowhere(
        &[
            KEPT_KEY,
            UNKNOWN_KEY,
            UNANSWERED_KEY,
            "and-carried-by-no-request",
        ],
        &said,
        &lines.written(),
    );
}

/// A person links a provider and types a first key the provider turns away.
/// The key is not kept: they are told the provider did not accept it, and
/// the provider still needs a key, so nothing shows it as added. A first
/// key the provider gives no answer about is kept, since none is lost by
/// it. No answer carries a key.
#[tokio::test]
async fn a_first_key_the_provider_turns_away_is_not_kept() {
    const UNKNOWN_KEY: &str = "sk-made-up-and-unknown-there";

    let dir = tempfile::tempdir().unwrap();
    let (_server, workspaces, workspace_id, service, ada) = served(dir.path()).await;
    workspaces.call_private_model_addresses(true);
    let (_stand_in, endpoint) = stand_in(&[]);
    let added = ask_own(
        &service,
        workspace_id,
        &ada,
        person_models::ADD_PROVIDER,
        json!({"name": "Stand-in", "protocol": "open-ai-compatible", "endpoint": endpoint}),
    )
    .await
    .expect("the person adds a provider");
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    let setting = |key: &'static str| {
        ask_own(
            &service,
            workspace_id,
            &ada,
            person_models::SET_KEY,
            json!({"provider": provider, "key": key}),
        )
    };
    let configured = || async {
        let hers = ask_own(&service, workspace_id, &ada, person_models::LIST, json!({}))
            .await
            .unwrap();
        hers["providers"][0]["configured"].clone()
    };

    let answered = setting(UNKNOWN_KEY).await;
    let Err(ActionCallError::Refused { reason }) = &answered else {
        panic!("a first key the provider turns away was kept: {answered:?}");
    };
    assert_eq!(reason, "Stand-in did not accept this key.");
    assert_eq!(configured().await, json!(false), "it still needs a key");

    let kept = setting(UNANSWERED_KEY)
        .await
        .expect("a first key nobody could say of is kept");
    assert_eq!(kept["configured"], json!(true), "{kept}");
    carried_nowhere(
        &[UNKNOWN_KEY, UNANSWERED_KEY],
        &[reason.clone(), kept.to_string()],
        "",
    );
}

/// A key the provider takes is asked about once, and then it is the key
/// their model is asked with. The kept key typed again replaces nothing,
/// and nobody is asked about it. Neither answer carries a key.
#[tokio::test]
async fn a_key_the_provider_takes_is_asked_about_once_and_replaces_the_kept_key() {
    const KEPT_KEY: &str = "sk-made-up-and-kept-first";
    const NEXT_KEY: &str = "sk-made-up-and-taken-next";

    let dir = tempfile::tempdir().unwrap();
    let (_server, workspaces, workspace_id, service, ada) = served(dir.path()).await;
    let scripted = ScriptedProvider::answering([]);
    workspaces.answer_models_with(scripted.clone());
    let (stand_in, endpoint) = stand_in(&[KEPT_KEY, NEXT_KEY]);
    let provider = linked_at_stand_in(
        &workspaces,
        (&service, workspace_id, &ada),
        (&endpoint, KEPT_KEY),
    )
    .await;
    let keeping = || {
        ask_own(
            &service,
            workspace_id,
            &ada,
            person_models::SET_KEY,
            json!({"provider": provider, "key": NEXT_KEY}),
        )
    };
    stand_in.drain_requests();

    let replaced = keeping().await.expect("a key the provider takes is kept");
    assert_eq!(replaced["configured"], json!(true), "{replaced}");
    let shown = stand_in.drain_requests();
    assert_eq!(shown.len(), 1, "the provider is asked about a new key once");
    assert_eq!(
        shown[0].header("authorization"),
        Some(format!("Bearer {NEXT_KEY}").as_str())
    );
    assert_eq!(
        asked_with(&workspaces, &ada, &scripted).await.as_deref(),
        Some(NEXT_KEY)
    );

    let again = keeping().await.expect("the kept key typed again stays");
    assert!(
        stand_in.drain_requests().is_empty(),
        "nobody is asked about the kept key"
    );
    carried_nowhere(
        &[KEPT_KEY, NEXT_KEY],
        &[replaced.to_string(), again.to_string()],
        "",
    );
}

/// The same holds for a key kept in the vault and named by reference: an
/// item whose key the provider turns away does not take the kept key's
/// place, and an item whose key the provider takes does. Neither answer
/// carries a key.
#[tokio::test]
async fn a_vault_items_key_the_provider_does_not_take_leaves_the_kept_key_in_use() {
    const KEPT_KEY: &str = "sk-made-up-and-kept-first";
    const ITEM_KEY: &str = "sk-made-up-and-kept-as-an-item";
    const UNKNOWN_KEY: &str = "sk-made-up-and-unknown-there";

    let dir = tempfile::tempdir().unwrap();
    let (_server, workspaces, workspace_id, service, ada) = served(dir.path()).await;
    let scripted = ScriptedProvider::answering([]);
    workspaces.answer_models_with(scripted.clone());
    let (stand_in, endpoint) = stand_in(&[KEPT_KEY, ITEM_KEY]);
    let provider = linked_at_stand_in(
        &workspaces,
        (&service, workspace_id, &ada),
        (&endpoint, KEPT_KEY),
    )
    .await;
    let mut items = Vec::new();
    for secret in [UNKNOWN_KEY, ITEM_KEY] {
        let kept = ask_at(
            &service,
            workspace_id,
            &ada,
            person_vault::target(person_vault::ADD),
            json!({"kind": "key", "belongsTo": [stand_in.url()], "secret": secret}),
        )
        .await
        .expect("a key is kept in the person's vault");
        items.push(kept["reference"].clone());
    }
    let keeping = |item: &Value| {
        ask_at(
            &service,
            workspace_id,
            &ada,
            person_models::target(person_models::SET_KEY_FROM_ITEM),
            json!({"provider": provider, "key": item}),
        )
    };

    let answered = keeping(&items[0]).await;
    let Err(ActionCallError::Refused { reason }) = &answered else {
        panic!("an item's key the provider turns away was kept: {answered:?}");
    };
    assert_eq!(
        reason,
        "Stand-in did not accept this key. The key kept before is still in use."
    );
    assert_eq!(
        asked_with(&workspaces, &ada, &scripted).await.as_deref(),
        Some(KEPT_KEY)
    );

    let view = keeping(&items[1])
        .await
        .expect("an item's key the provider takes is kept");
    assert_eq!(view["configured"], json!(true), "{view}");
    assert_eq!(
        asked_with(&workspaces, &ada, &scripted).await.as_deref(),
        Some(ITEM_KEY)
    );
    for key in [KEPT_KEY, ITEM_KEY, UNKNOWN_KEY] {
        assert!(!reason.contains(key) && !view.to_string().contains(key));
    }
}

/// A computer logged in as the person acts as them: the person on it, and the agent it hands
/// its own session to (its apps, the person's own agent), ask the person's model.
#[tokio::test]
async fn the_person_and_their_own_computers_agent_ask_their_model() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, _) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let laptop = super::tests::caller(&ada);
    let own_agent = laptop
        .clone()
        .delegated_to(tilo_actions::remote::device_agent_id(
            &ada.user_public_key,
            "Claude Code",
        ));
    for (who, asking) in [("the person", laptop), ("her own agent", own_agent)] {
        assert_eq!(
            Workspaces::asking_for_model(&asking, Some(&ada), Some(workspace_id)),
            Ok(ada.user_public_key),
            "{who} asks her model"
        );
    }
}

/// An agent a grant admitted uses the person's model only as far as its grant says: asking
/// it directly answers nothing, whether a delegation admitted it on a device of the person's
/// or a key was minted for it.
#[tokio::test]
async fn an_agent_a_grant_admitted_does_not_ask_the_persons_model() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, _) = governed(workspaces).await;
    let granted_device = admit(workspaces, &workspace, &owner, 2).await;
    let granted = Caller::delegated_agent(
        "careful",
        granted_device.user_public_key,
        granted_device.device_public_key,
    );
    let mut key_cert = admit(workspaces, &workspace, &owner, 3).await;
    key_cert.capabilities.agent = Some(tilo_user_identity::AgentScope::AllWorkspaces);
    let keyed = super::tests::caller(&key_cert).delegated_to(tilo_action_policy::key_agent_id(
        &key_cert.device_public_key,
    ));
    for (who, asking, certificate) in [
        ("an agent a delegation admitted", granted, &granted_device),
        ("an agent a key was minted for", keyed, &key_cert),
    ] {
        assert_eq!(
            Workspaces::asking_for_model(&asking, Some(certificate), Some(workspace_id)),
            Err(ActionCallError::UnknownAction),
            "{who} is answered nothing"
        );
    }
}
