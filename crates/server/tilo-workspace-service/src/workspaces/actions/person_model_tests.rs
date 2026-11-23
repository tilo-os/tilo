//! A workspace's model from the store's own actions to the answer: what a
//! member keeps through Settings' actions on a device of theirs is what
//! answers work done in the workspace, a handed agent reads it and its
//! changes ask its member, nobody outside the workspace reaches it, and no
//! log line carries a key.

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
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::DeviceCert;

use super::ServiceWorkspaceActions;
use super::tests::{admit, ask_own, caller, governed, request, settled};
use crate::workspaces::fixtures::test_host;
use crate::workspaces::person_model::scripted::{Reply, ScriptedProvider};
use crate::workspaces::person_model::{Interaction, ModelRefusal};
use crate::workspaces::tests::governor;
use crate::workspaces::{AsksPeople, PersonAnswer, Workspaces};

const ADA_KEY: &str = "sk-ada-linked-through-settings";
const BO_KEY: &str = "sk-bo-linked-through-settings";
/// The key a member's agent would keep in place of the one the workspace
/// holds.
const REPLACED_KEY: &str = "sk-replaced-by-her-agent";

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

/// A person who answers every question one way, or not at all, and what
/// they were asked.
struct Asked {
    answer: Option<ConfirmationAnswer>,
    questions: Mutex<Vec<([u8; 32], PendingConfirmation)>>,
}

impl Asked {
    fn answering(answer: Option<ConfirmationAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answer,
            questions: Mutex::default(),
        })
    }

    /// How many questions the person was put.
    fn count(&self) -> usize {
        self.questions.lock().unwrap().len()
    }
}

impl AsksPeople for Asked {
    fn ask(&self, person: [u8; 32], question: PendingConfirmation) -> PersonAnswer {
        self.questions.lock().unwrap().push((person, question));
        let answer = self
            .answer
            .clone()
            .ok_or_else(|| "nobody answered".to_owned());
        Box::pin(async move { answer })
    }
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

/// The key the workspace's model is asked with now: one call of it, which
/// `scripted` answers.
async fn asked_with(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    scripted: &ScriptedProvider,
) -> Option<String> {
    scripted.then(Reply::text("an answer"));
    workspaces
        .workspace_model(workspace, Interaction::Request)
        .answer(&asking("Hello?"), soon(), &mut |_| {})
        .await
        .expect("the workspace's model answers");
    scripted.asked().last().and_then(|asked| asked.key.clone())
}

/// A workspace at a server that reaches the stand-in provider, its provider
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
    let provider = host.in_workspace_as(workspace_id, device);
    let invocation = request.invocation;
    let mut receipt = settled(provider.invoke(caller, request)).await?;
    // An ask of the workspace's model is accepted, then followed until it
    // ends.
    if matches!(receipt.status, ReceiptStatus::Accepted) {
        let now = tilo_clock::now();
        let until = now.checked_add(tilo_test_env::hang_bound()).unwrap_or(now);
        receipt = settled(tilo_actions::remote::settle_with(
            &provider, caller, invocation, None, until,
        ))
        .await
        .expect("the ask is the caller's to follow")
        .receipt();
    }
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("the workspace's own settings answer at once: {other:?}"),
    }
}

/// The member `cy` of another workspace links a provider of its own there and chooses its model
/// for requests.
async fn another_workspace_links_its_own(
    workspaces: &Workspaces,
    host: &ServiceWorkspaceActions,
    elsewhere_id: WorkspaceId,
    cy: &DeviceCert,
) {
    let theirs = link(
        host,
        elsewhere_id,
        cy,
        ("Cy's own", "https://models.example.com/v1", BO_KEY),
    )
    .await;
    workspaces
        .offer_models_for(&elsewhere_id.to_string(), &theirs, &["open-model"])
        .unwrap();
    ask_own(
        host,
        elsewhere_id,
        cy,
        person_models::SET_ASSIGNMENT,
        json!({"kind": "request", "provider": theirs, "model": "open-model"}),
    )
    .await
    .expect("the other workspace chooses its model");
}

/// The members of one workspace link a provider between them, as their own
/// devices, through the actions Settings asks with: the one model it linked
/// answers work done in the workspace, asked with the one key it keeps, and
/// a kind nobody chose a model for is refused. Another workspace on the
/// same host is answered by the model it linked, never by this one's.
#[tokio::test]
async fn a_workspace_is_answered_by_the_one_model_it_linked_and_another_by_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    let scripted = ScriptedProvider::answering([
        Reply::text("for the workspace").costing(700),
        Reply::text("for the other"),
    ]);
    workspaces.answer_models_with(scripted.clone());

    // Ada links the provider her workspace keeps and chooses the model its
    // work runs on, through the actions Settings asks with. The provider's
    // own list of models, which the host reads with the key once that
    // lands; until then a provider added from a page offers none, so the
    // list is written here.
    let provider = link(
        &host,
        workspace_id,
        &ada,
        (
            "Google",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            ADA_KEY,
        ),
    )
    .await;
    workspaces
        .offer_models_for(&workspace_id.to_string(), &provider, &["gemini-main"])
        .unwrap();
    let chosen = ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::SET_ASSIGNMENT,
        json!({"kind": "request", "provider": provider, "model": "gemini-main"}),
    )
    .await
    .expect("a member chooses a model for a kind of work");
    assert_eq!(chosen["assignments"][0]["model"], json!("gemini-main"));

    // Another workspace's member links its own.
    another_workspace_links_its_own(workspaces, &host, elsewhere_id, &cy).await;

    // The other member of the one workspace reads what it keeps, and one
    // model answers work there for both.
    let his = ask_own(&host, workspace_id, &bo, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(his["providers"][0]["id"], json!(provider), "{his}");
    assert_eq!(
        his["assignments"][0]["model"],
        json!("gemini-main"),
        "{his}"
    );
    let answered = workspaces
        .workspace_model(workspace_id, Interaction::Request)
        .answer(&asking("Good morning"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(answered.linked.provider, "Google");
    assert_eq!(answered.response.cost_micros, 700);
    let other = workspaces
        .workspace_model(elsewhere_id, Interaction::Request)
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(other.linked.provider, "Cy's own");
    assert!(
        matches!(other.response.message.content.as_slice(), [Content::Text { text }] if text == "for the other")
    );
    let asked = scripted.asked();
    assert_eq!(
        asked
            .iter()
            .map(|asked| (asked.model.as_str(), asked.key.as_deref()))
            .collect::<Vec<_>>(),
        vec![("gemini-main", Some(ADA_KEY)), ("open-model", Some(BO_KEY))]
    );

    // Neither workspace chose a model for judging, and neither has a
    // default: no other model answers in its place.
    let refused = workspaces
        .workspace_model(workspace_id, Interaction::Classifier)
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

/// Her yes to `agent` replaces the key of her `provider`, and still no answer carries a key.
async fn her_yes_replaces_the_key(
    workspaces: &Workspaces,
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    ada: &DeviceCert,
    agent: &Caller,
    provider: &str,
) {
    let allowing = Asked::answering(Some(ConfirmationAnswer::AllowOnce));
    workspaces.ask_people(allowing.clone());
    let replaced = ask_as(
        host,
        workspace_id,
        ada,
        agent,
        (
            person_models::SET_KEY,
            json!({"provider": provider, "key": REPLACED_KEY}),
        ),
    )
    .await
    .expect("she allowed the change");
    assert_eq!(replaced["configured"], json!(true), "{replaced}");
    assert!(!replaced.to_string().contains(REPLACED_KEY));
    assert_eq!(allowing.count(), 1, "one question for one change");
}

/// A member of another workspace and a device of nobody (`strangers`), a local agent
/// and an app are answered nothing by `ada`'s store: no listing, no key either way, and none of
/// the `changes`.
async fn nobody_else_is_answered(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    ada: &DeviceCert,
    strangers: [&DeviceCert; 2],
    changes: &[(&'static str, Value)],
) {
    let person = hex::encode(ada.user_public_key);
    let nobodies = [
        Caller::local_agent("helper"),
        Caller::local_app("so.tilo.chat").acting_for(person),
    ];
    for nobody in &nobodies {
        for asking in [
            (person_models::LIST, json!({})),
            (
                person_models::ASK,
                serde_json::to_value(asking("Spend her key")).unwrap(),
            ),
        ]
        .into_iter()
        .chain(changes.iter().cloned())
        {
            let verb = asking.0;
            let refused = ask_as(host, workspace_id, ada, nobody, asking).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{verb} as {:?}: {refused:?}",
                nobody.acting()
            );
        }
    }
    let [other_member, nobodys_device] = strangers;
    for (who, device) in [
        ("a member of another workspace", other_member),
        ("a device of nobody", nobodys_device),
    ] {
        for asking in [(person_models::LIST, json!({}))]
            .into_iter()
            .chain(changes.iter().cloned())
        {
            let verb = asking.0;
            let refused = ask_as(host, workspace_id, device, &caller(device), asking).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{who}, {verb}: {refused:?}"
            );
        }
    }
}

/// An agent a member's device handed its session to reads the workspace's
/// providers, and never a key: what it reads back says only that one is
/// kept. Its changes ask that member first, and are made only when they
/// allow them. A member of another workspace, a bare agent turn with no
/// device of a member's behind it and an app are nobody to the store.
#[tokio::test]
async fn a_handed_agent_reads_the_workspaces_listing_and_its_key_changes_ask() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    workspaces.call_private_model_addresses(true);
    // A provider on this machine that takes the key it holds and the one
    // her agent would keep in its place.
    let (stand_in, endpoint) = stand_in(&[ADA_KEY, REPLACED_KEY]);
    let provider = link(&host, workspace_id, &ada, ("Stand-in", &endpoint, ADA_KEY)).await;
    workspaces
        .offer_models_for(&workspace_id.to_string(), &provider, &["gemini-main"])
        .unwrap();
    let (elsewhere, _, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 2).await;
    let (_, stranger) = governor();
    let changes = || {
        [
            (
                person_models::SET_KEY,
                json!({"provider": provider, "key": REPLACED_KEY}),
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
        ]
    };

    // The agent her own computer hands its session to reads the listing,
    // which says a key is kept and carries none.
    let agent = caller(&ada).delegated_to("helper");
    let listed = ask_as(
        &host,
        workspace_id,
        &ada,
        &agent,
        (person_models::LIST, json!({})),
    )
    .await
    .unwrap();
    assert_eq!(
        listed["providers"][0]["configured"],
        json!(true),
        "{listed}"
    );
    assert!(!listed.to_string().contains(ADA_KEY), "{listed}");

    // Every change it would make asks her first: unanswered none is made,
    // and her no refuses it.
    for (verb, argument) in changes() {
        let unasked = ask_as(&host, workspace_id, &ada, &agent, (verb, argument.clone())).await;
        assert!(
            matches!(unasked, Err(ActionCallError::ConfirmationRequired { .. })),
            "{verb} with nobody to ask through: {unasked:?}"
        );
    }
    workspaces.ask_people(Asked::answering(Some(ConfirmationAnswer::Deny)));
    for (verb, argument) in changes() {
        let refused = ask_as(&host, workspace_id, &ada, &agent, (verb, argument)).await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{verb} she refused: {refused:?}"
        );
    }
    let hers = ask_own(&host, workspace_id, &ada, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(hers["providers"][0]["configured"], json!(true), "{hers}");
    assert_eq!(hers["assignments"], Value::Null, "nothing changed");

    // Her yes replaces the key, and the provider is asked about the new one once.
    stand_in.drain_requests();
    her_yes_replaces_the_key(workspaces, &host, workspace_id, &ada, &agent, &provider).await;
    let asked = stand_in.drain_requests();
    assert_eq!(
        asked.len(),
        1,
        "the provider is asked about the new key once"
    );
    assert_eq!(
        asked[0].header("authorization"),
        Some(format!("Bearer {REPLACED_KEY}").as_str())
    );
    nobody_else_is_answered(&host, workspace_id, &ada, [&cy, &stranger], &changes()).await;

    // The workspace's key is still the one its model is asked with.
    let scripted = ScriptedProvider::answering([Reply::text("done")]);
    workspaces.answer_models_with(scripted.clone());
    workspaces
        .choose_model_for(
            &workspace_id.to_string(),
            Interaction::Task,
            &provider,
            "gemini-main",
        )
        .unwrap();
    workspaces
        .workspace_model(workspace_id, Interaction::Task)
        .answer(&asking("Do it"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(scripted.asked()[0].key.as_deref(), Some(REPLACED_KEY));
    server.shutdown().await;
}

/// A page's window asks the workspace's model through its host: the turn is
/// answered by the model a member set up in Settings, with the key the
/// workspace's vault keeps, and a workspace that set none up is told so, in
/// a failure whose kind the window decides by.
#[tokio::test]
async fn a_page_asks_the_workspaces_model_through_its_host() {
    use agent_provider_config::protocol::{ErrorKind, ModelError, ModelResponse};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    let scripted = ScriptedProvider::answering([
        Reply::text("from the server"),
        Reply::text("from the server too"),
    ]);
    workspaces.answer_models_with(scripted.clone());

    let provider = link(
        &host,
        workspace_id,
        &ada,
        ("Google", "https://models.example.com/v1", ADA_KEY),
    )
    .await;
    workspaces
        .offer_models_for(&workspace_id.to_string(), &provider, &["gemini-main"])
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

    // The other member's page is answered by the same model.
    let his = ask_own(&host, workspace_id, &bo, person_models::ASK, turn.clone())
        .await
        .unwrap();
    let his: Result<ModelResponse, ModelError> = serde_json::from_value(his).unwrap();
    let his = his.unwrap();
    assert!(
        matches!(his.message.content.as_slice(), [Content::Text { text }] if text == "from the server too")
    );
    assert_eq!(
        scripted.asked().len(),
        2,
        "the workspace's one model answers"
    );

    // A page in another workspace is answered by none.
    let theirs = ask_own(&host, elsewhere_id, &cy, person_models::ASK, turn)
        .await
        .unwrap();
    let theirs: Result<ModelResponse, ModelError> = serde_json::from_value(theirs).unwrap();
    assert_eq!(theirs.unwrap_err().kind, ErrorKind::NoProvider);
    assert_eq!(
        scripted.asked().len(),
        2,
        "no other workspace's model answers"
    );
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
        .offer_models_for(&workspace_id.to_string(), &provider, &["gemini-main"])
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
        .offer_models_for(&workspace_id.to_string(), &provider, &["gemini-main"])
        .unwrap();
    workspaces
        .choose_model_for(
            &workspace_id.to_string(),
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
    let model = workspaces.workspace_model(workspace_id, Interaction::Request);
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
        asked_with(&workspaces, workspace_id, &scripted)
            .await
            .as_deref(),
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
            asked_with(&workspaces, workspace_id, &scripted)
                .await
                .as_deref(),
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
        asked_with(&workspaces, workspace_id, &scripted)
            .await
            .as_deref(),
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
        asked_with(&workspaces, workspace_id, &scripted)
            .await
            .as_deref(),
        Some(KEPT_KEY)
    );

    let view = keeping(&items[1])
        .await
        .expect("an item's key the provider takes is kept");
    assert_eq!(view["configured"], json!(true), "{view}");
    assert_eq!(
        asked_with(&workspaces, workspace_id, &scripted)
            .await
            .as_deref(),
        Some(ITEM_KEY)
    );
    for key in [KEPT_KEY, ITEM_KEY, UNKNOWN_KEY] {
        assert!(!reason.contains(key) && !view.to_string().contains(key));
    }
}

/// A computer logged in as a member acts as them: the member on it, and the
/// agent it hands its own session to (its apps, the member's own agent), ask
/// the workspace's model through the action a page's window asks with.
#[tokio::test]
async fn the_member_and_their_own_computers_agent_ask_the_workspaces_model() {
    use agent_provider_config::protocol::{ModelError, ModelResponse};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let scripted = ScriptedProvider::answering([
        Reply::text("from the workspace"),
        Reply::text("from the workspace"),
    ]);
    workspaces.answer_models_with(scripted.clone());
    let provider = link(
        &host,
        workspace_id,
        &ada,
        ("Google", "https://models.example.com/v1", ADA_KEY),
    )
    .await;
    workspaces
        .offer_models_for(&workspace_id.to_string(), &provider, &["gemini-main"])
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

    let laptop = caller(&ada);
    let own_agent = laptop
        .clone()
        .delegated_to(tilo_actions::remote::device_agent_id(
            &ada.user_public_key,
            "Claude Code",
        ));
    let turn = serde_json::to_value(asking("Is it time?")).unwrap();
    for (who, asking) in [("the member", laptop), ("their own agent", own_agent)] {
        let answered = ask_as(
            &host,
            workspace_id,
            &ada,
            &asking,
            (person_models::ASK, turn.clone()),
        )
        .await
        .unwrap_or_else(|refused| panic!("{who} asks it: {refused:?}"));
        let answered: Result<ModelResponse, ModelError> = serde_json::from_value(answered).unwrap();
        assert!(
            matches!(answered.unwrap().message.content.as_slice(), [Content::Text { text }] if text == "from the workspace"),
            "{who}"
        );
    }
    assert_eq!(scripted.asked().len(), 2, "one model answered both");
    server.shutdown().await;
}

/// An agent no member's device stands behind asks the workspace's model
/// nothing: not one the server admitted itself, not an
/// app, and not an agent whose device is no member's.
#[tokio::test]
async fn an_agent_no_members_device_stands_behind_asks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let scripted = ScriptedProvider::answering([Reply::text("never said")]);
    workspaces.answer_models_with(scripted.clone());
    let person = hex::encode(ada.user_public_key);
    let (_, stranger) = governor();
    let outside = Caller::delegated_agent(
        "careful",
        stranger.user_public_key,
        stranger.device_public_key,
    );
    let asked = serde_json::to_value(asking("Spend the workspace's key")).unwrap();
    for (who, device, nobody) in [
        (
            "an agent the server admitted itself",
            &ada,
            Caller::local_agent("helper"),
        ),
        (
            "an app",
            &ada,
            Caller::local_app("so.tilo.chat").acting_for(person),
        ),
        ("an agent whose device is no member's", &stranger, outside),
    ] {
        for verb in [person_models::ASK, person_models::LIST] {
            let argument = if verb == person_models::ASK {
                asked.clone()
            } else {
                json!({})
            };
            let refused = ask_as(&host, workspace_id, device, &nobody, (verb, argument)).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{who}, {verb}: {refused:?}"
            );
        }
    }
    assert!(scripted.asked().is_empty(), "no provider was asked");
    server.shutdown().await;
}
