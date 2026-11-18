//! A person's model from the store's own actions to the answer: what their
//! own device keeps through Settings' actions is what answers work done for
//! them, no agent reads or changes any of it, and no log line carries a key.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives"
)]

use std::sync::{Arc, Mutex};

use agent_provider_config::protocol::{Content, Message, ModelRequest, Role, ToolChoice};
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, ReceiptStatus, person_models,
};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::HostWorkspaceActions;
use super::tests::{admit, ask_own, governed, request, settled};
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

/// `device`'s person adds a provider and keeps its key, through the actions
/// Settings asks with: the provider's id.
async fn link(
    host: &HostWorkspaceActions,
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
    host: &HostWorkspaceActions,
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
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let said = "one two three four five six seven eight";
    workspaces.answer_models_with(ScriptedProvider::answering([Reply::text(said)]));
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

    let actions = host.in_workspace_as(workspace_id, &ada);
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

    let lines = Lines::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(lines.clone())
        .finish();
    let _logged = tracing::subscriber::set_default(subscriber);

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

    let written = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(
        written.contains("a person's model did not answer"),
        "the log was read: {written}"
    );
    for secret in [ADA_KEY, BO_KEY] {
        assert!(!written.contains(secret), "a key was logged: {written}");
    }
}
