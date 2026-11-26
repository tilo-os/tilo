//! A workspace's model, from the workspace's store to the provider's answer:
//! which model a kind of work runs on, every reason nothing answers, the
//! stream, the cost, and a call that is stopped.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the host handle a test binds is kept until the test ends; dropping it earlier closes the host it drives"
)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_provider_config::protocol::{Content, Message, Role, StreamStatus, ToolChoice};

use super::*;
use crate::fixtures::{hosting_device, test_host};
use crate::model_answers::scripted::{Linking, Reply, ScriptedProvider};
use crate::person_settings::save_provider_config;

const ADA: WorkspaceId = WorkspaceId::from_u128(7);
const BO: WorkspaceId = WorkspaceId::from_u128(9);
const ADA_KEY: &str = "sk-ada-main-thread";
const ADA_SECOND_KEY: &str = "sk-ada-parts";
const BO_KEY: &str = "sk-bo-own";

const GOOGLE: Linking<'static> = Linking {
    name: "Google",
    protocol: ProviderProtocol::OpenAiCompatible,
    endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
    key: Some(ADA_KEY),
    models: &["gemini-main", "gemini-small"],
};
const ANTHROPIC: Linking<'static> = Linking {
    name: "Anthropic",
    protocol: ProviderProtocol::Anthropic,
    endpoint: "https://api.anthropic.com",
    key: Some(ADA_SECOND_KEY),
    models: &["claude-parts"],
};

fn asking(text: &str) -> ModelRequest {
    ModelRequest {
        messages: vec![Message::text(Role::User, text)],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 256,
    }
}

/// A deadline no answer of a test reaches.
fn soon() -> Instant {
    tilo_clock::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap()
}

fn text_of(answer: &Answer) -> String {
    answer
        .response
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A workspace that linked nothing is refused in words people can act on,
/// the provider is never asked, and asking keeps no store for it.
#[tokio::test]
async fn nothing_linked_is_refused_and_asks_no_provider() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering([Reply::text("never said")]);
    workspaces.answer_models_with(scripted.clone());

    let model = workspaces.workspace_model(ADA, Interaction::Task);
    let refused = model.linked().unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::NoModelLinked {
            kind: Interaction::Task
        }
    );
    assert_eq!(refused.code(), "no-model-linked");
    assert_eq!(
        refused.words(None),
        "No model is linked for this kind of work. Choose one in Settings, under Agent models."
    );
    assert_eq!(
        refused.words(Some("de-AT")),
        "Für diese Art von Arbeit ist kein Modell verknüpft. Wähle eines in den Einstellungen unter Agentenmodelle."
    );
    assert_eq!(refused.model_error().kind, ErrorKind::NoProvider);

    let answered = model.answer(&asking("Hello"), soon(), &mut |_| {}).await;
    assert_eq!(answered.unwrap_err(), refused);
    assert!(scripted.asked().is_empty(), "no provider is asked");
    assert!(
        !workspaces.workspace_root(&ADA.to_string()).exists(),
        "asking keeps no store for a workspace that set nothing up"
    );
}

/// A kind of work runs on the model chosen for it and every other kind on
/// the workspace's default, each asked with its own provider's key; another
/// workspace on the same host is answered by its own.
#[tokio::test]
async fn a_kind_runs_on_its_own_model_and_the_rest_on_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering([
        Reply::text("main thread"),
        Reply::text("a part"),
        Reply::text("for Bo"),
    ]);
    workspaces.answer_models_with(scripted.clone());

    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let parts = workspaces
        .link_model_for(&ADA.to_string(), &ANTHROPIC, false)
        .await
        .unwrap();
    workspaces
        .choose_model_for(
            &ADA.to_string(),
            Interaction::Subtask,
            &parts,
            "claude-parts",
        )
        .unwrap();
    let bo = Linking {
        name: "Bo's own",
        endpoint: "https://models.example.com/v1",
        key: Some(BO_KEY),
        models: &["open-model"],
        ..GOOGLE
    };
    workspaces
        .create_workspace("Bo", Some(&BO.to_string()))
        .await
        .unwrap();
    workspaces
        .link_model_for(&BO.to_string(), &bo, true)
        .await
        .unwrap();

    let main = workspaces
        .workspace_model(ADA, Interaction::Request)
        .answer(&asking("Good morning"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(text_of(&main), "main thread");
    assert_eq!(main.linked.provider, "Google");
    assert_eq!(main.linked.model.id, "gemini-main");
    let part = workspaces
        .workspace_model(ADA, Interaction::Subtask)
        .answer(&asking("Look this up"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(part.linked.provider, "Anthropic");
    assert_eq!(part.linked.protocol, ProviderProtocol::Anthropic);
    let theirs = workspaces
        .workspace_model(BO, Interaction::Request)
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(theirs.linked.provider, "Bo's own");

    let asked = scripted.asked();
    let seen: Vec<(&str, &str, Option<&str>)> = asked
        .iter()
        .map(|asked| {
            (
                asked.endpoint.as_str(),
                asked.model.as_str(),
                asked.key.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (GOOGLE.endpoint, "gemini-main", Some(ADA_KEY)),
            (ANTHROPIC.endpoint, "claude-parts", Some(ADA_SECOND_KEY)),
            ("https://models.example.com/v1", "open-model", Some(BO_KEY)),
        ]
    );
    assert_eq!(asked[1].protocol, ProviderProtocol::Anthropic);
    assert_eq!(
        asked[0].request,
        asking("Good morning"),
        "the request reaches the provider as it was asked"
    );
}

/// A model chosen for a kind that can no longer be used is a refusal that
/// names it: the workspace's default does not answer in its place.
#[tokio::test]
async fn a_choice_that_cannot_be_used_is_refused_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering([Reply::text("the default")]);
    workspaces.answer_models_with(scripted.clone());
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let parts = workspaces
        .link_model_for(&ADA.to_string(), &ANTHROPIC, false)
        .await
        .unwrap();
    workspaces
        .choose_model_for(&ADA.to_string(), Interaction::Task, &parts, "claude-parts")
        .unwrap();

    // A member switches the provider off.
    let root = workspaces.workspace_root(&ADA.to_string());
    let mut config = provider_config(&root);
    config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == parts)
        .unwrap()
        .enabled = false;
    save_provider_config(&root, &config).unwrap();

    let refused = workspaces
        .workspace_model(ADA, Interaction::Task)
        .answer(&asking("Do this"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::ChoiceUnusable {
            kind: Interaction::Task,
            provider: "Anthropic".into(),
            model: "claude-parts".into(),
        }
    );
    assert_eq!(
        refused.words(None),
        "claude-parts from Anthropic can no longer be used. Choose another model in Settings."
    );
    assert!(scripted.asked().is_empty(), "the default is not asked");
    // Every other kind still runs on the default.
    assert_eq!(
        workspaces
            .workspace_model(ADA, Interaction::Request)
            .linked()
            .unwrap()
            .model
            .id,
        "gemini-main"
    );
}

/// A provider kept without its key answers nothing, and says which key is
/// missing.
#[tokio::test]
async fn a_provider_without_its_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let keyless = Linking {
        key: None,
        ..GOOGLE
    };
    workspaces
        .link_model_for(&ADA.to_string(), &keyless, true)
        .await
        .unwrap();

    let refused = workspaces
        .workspace_model(ADA, Interaction::Request)
        .linked()
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::NoKey {
            provider: "Google".into()
        }
    );
    assert_eq!(
        refused.words(None),
        "Google has no API key here yet. Add it in Settings."
    );
    assert_eq!(refused.model_error().kind, ErrorKind::Authentication);
}

/// A provider at an address off the public internet, or one without TLS, is
/// called only on a host whose operator allows it: a workspace's settings
/// never point the host at its own network.
#[tokio::test]
async fn an_address_off_the_public_internet_needs_the_operators_word() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    for (workspace, endpoint) in [
        (WorkspaceId::from_u128(1), "http://127.0.0.1:8000/v1"),
        (WorkspaceId::from_u128(2), "https://10.0.0.5/v1"),
        (WorkspaceId::from_u128(3), "https://models.lan/v1"),
        (WorkspaceId::from_u128(4), "http://models.example.com/v1"),
        (WorkspaceId::from_u128(5), "https://localhost/v1"),
    ] {
        let local = Linking {
            name: "A model server",
            endpoint,
            key: None,
            ..GOOGLE
        };
        workspaces
            .link_model_for(&workspace.to_string(), &local, true)
            .await
            .unwrap();
        let model = workspaces.workspace_model(workspace, Interaction::Request);
        assert_eq!(
            model.linked().unwrap_err(),
            ModelRefusal::AddressRefused {
                provider: "A model server".into()
            },
            "{endpoint}"
        );
    }
    assert_eq!(
        workspaces
            .workspace_model(WorkspaceId::from_u128(1), Interaction::Request)
            .linked()
            .unwrap_err()
            .model_error()
            .kind,
        ErrorKind::PermissionDenied
    );

    // The operator's own network, and a server there that takes no key.
    workspaces.call_private_model_addresses(true);
    let linked = workspaces
        .workspace_model(WorkspaceId::from_u128(1), Interaction::Request)
        .linked()
        .unwrap();
    assert_eq!(linked.provider, "A model server");
}

/// The answer arrives as it streams and whole at its end, with what it
/// cost.
#[tokio::test]
async fn the_answer_streams_and_says_what_it_cost() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering([
        Reply::text("Good morning, Ada. Two things wait for you.").costing(1_250),
        Reply::tool_call("call-1", "invoke", serde_json::json!({"action": "post"})).costing(90),
    ]);
    workspaces.answer_models_with(scripted);
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let model = workspaces.workspace_model(ADA, Interaction::Request);

    let mut stream = protocol::StreamState::default();
    let mut pieces = 0_usize;
    let answer = model
        .answer(&asking("Good morning"), soon(), &mut |event| {
            pieces = pieces.saturating_add(1);
            stream.apply(&event);
        })
        .await
        .unwrap();
    assert!(pieces > 1, "the words arrive in pieces, not at the end");
    assert_eq!(stream.status, StreamStatus::Streaming);
    assert_eq!(stream.text, "Good morning, Ada. Two things wait for you.");
    assert_eq!(text_of(&answer), stream.text);
    assert_eq!(answer.response.cost_micros, 1_250);
    assert!(answer.response.usage.output_tokens > 0);

    // A tool call comes back whole in the answer; what streamed of it is for
    // showing only.
    let mut deltas = Vec::new();
    let calling = model
        .answer(&asking("Post it"), soon(), &mut |event| deltas.push(event))
        .await
        .unwrap();
    let calls = calling.response.tool_calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "invoke");
    assert_eq!(calls[0].input, serde_json::json!({"action": "post"}));
    assert!(matches!(
        deltas.as_slice(),
        [StreamEvent::ToolCallDelta { name: Some(name), .. }] if name == "invoke"
    ));
    assert_eq!(calling.response.cost_micros, 90);
}

/// A provider's own failure is the refusal, in the words it is safe to
/// show. Where the provider turns the workspace's key away, or the key's
/// account, the refusal names the provider and says what a member can do
/// about it, in their language; the failure keeps the provider's kind.
#[tokio::test]
async fn a_providers_failure_is_the_refusal_and_a_refused_key_names_its_provider() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let of_the_key = [
        (
            ErrorKind::Authentication,
            "Google did not accept your API key. Check the key, or use another key for Google.",
            "Google hat deinen API-Schlüssel nicht angenommen. Prüfe den Schlüssel oder verwende einen anderen Schlüssel für Google.",
        ),
        (
            ErrorKind::PermissionDenied,
            "Google does not let your API key do this. Check what the key may do at Google, or use another key.",
            "Google lässt deinen API-Schlüssel das nicht tun. Prüfe bei Google, was der Schlüssel darf, oder verwende einen anderen Schlüssel.",
        ),
        (
            ErrorKind::CreditsExhausted,
            "The account of your Google key has no credit left. Add credit at Google, or use another key.",
            "Das Konto deines Schlüssels für Google hat kein Guthaben mehr. Lade bei Google Guthaben auf oder verwende einen anderen Schlüssel.",
        ),
    ];
    workspaces.answer_models_with(ScriptedProvider::answering(
        of_the_key
            .iter()
            .map(|(kind, ..)| *kind)
            .chain([ErrorKind::Unavailable])
            .map(|kind| Reply::failing(ModelError::new(kind))),
    ));
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let model = workspaces.workspace_model(ADA, Interaction::Request);

    for (kind, english, german) in of_the_key {
        let refused = model
            .answer(&asking("Hello"), soon(), &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "provider", "{kind:?}");
        assert_eq!(refused.words(None), english);
        assert_eq!(refused.words(Some("de-AT")), german);
        let error = refused.model_error();
        assert_eq!(error.kind, kind, "the failure keeps the provider's kind");
        assert_eq!(error.message, english);
        assert!(!format!("{refused:?} {refused}").contains(ADA_KEY));
    }
    // Every other failure of the provider's says what it said, and names nobody.
    let refused = model
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "provider");
    assert_eq!(
        refused,
        ModelRefusal::Provider(ModelError::new(ErrorKind::Unavailable))
    );
    assert_eq!(refused.words(None), "Model service unavailable");
}

/// A provider that ran out of time, was stopped, cut its answer off, declined, ran out of room,
/// was busy or could not do what was asked is said in the person's words and language, never as
/// the gateway's diagnostic.
#[test]
fn each_failure_of_a_provider_reads_in_the_persons_language() {
    for kind in [
        ErrorKind::TimedOut,
        ErrorKind::Cancelled,
        ErrorKind::Truncated,
        ErrorKind::Refused,
        ErrorKind::ContextLimit,
        ErrorKind::RateLimited,
        ErrorKind::Unsupported,
    ] {
        let diagnostic = ModelError::new(kind);
        let refused = ModelRefusal::Provider(diagnostic.clone());
        let (english, german) = (refused.words(None), refused.words(Some("de-DE")));
        assert_ne!(
            english, diagnostic.message,
            "{kind:?} has a sentence of its own"
        );
        assert_ne!(german, english, "{kind:?} reads in German");
        assert!(!german.starts_with("model-"), "{kind:?}: {german}");
        let error = refused.model_error_in(Some("de-DE"));
        assert_eq!(error.kind, kind, "the failure keeps its kind");
        assert_eq!(error.message, german);
    }
}

/// A host that was given nothing to speak to providers with answers no
/// call, and never with a model of somebody else's.
#[tokio::test]
async fn a_host_that_speaks_to_no_provider_answers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    workspaces.answer_models_with(Arc::new(NoProvider));
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let refused = workspaces
        .workspace_model(ADA, Interaction::Request)
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::Provider(ModelError::new(ErrorKind::Unavailable))
    );
}

/// A call ends at its deadline, and a call a thread waits for ends when it
/// is stopped: neither waits for a provider that never answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_ends_at_its_deadline_or_when_it_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering([Reply::never(), Reply::never(), Reply::never()]);
    workspaces.answer_models_with(scripted.clone());
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let model = workspaces.workspace_model(ADA, Interaction::Task);

    let due = tilo_clock::now()
        .checked_add(std::time::Duration::from_millis(50))
        .unwrap();
    let late = model.answer(&asking("Work"), due, &mut |_| {}).await;
    assert_eq!(
        late.unwrap_err(),
        ModelRefusal::Provider(ModelError::new(ErrorKind::TimedOut))
    );

    // A thread of its own waits for the answer, as an agent's does, and is
    // told to stop once the provider has the call: the flag a turn already
    // holds is all it takes.
    let stop = Arc::new(AtomicBool::new(false));
    let (waiting, stopping) = (model.clone(), Arc::clone(&stop));
    let waited = tokio::task::spawn_blocking(move || {
        waiting.answer_blocking(
            &asking("Work"),
            soon(),
            &|| stopping.load(Ordering::Acquire),
            &mut |_| {},
        )
    });
    tilo_test_env::within("the provider is asked", async {
        while scripted.asked().len() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    stop.store(true, Ordering::Release);
    let stopped = tilo_test_env::within("the stopped call returns", waited)
        .await
        .unwrap();
    assert_eq!(
        stopped.unwrap_err(),
        ModelRefusal::Provider(ModelError::new(ErrorKind::Cancelled))
    );

    // A call stopped before it starts never waits for the provider's answer.
    let again = model.clone();
    let stopped = tokio::task::spawn_blocking(move || {
        again.answer_blocking(&asking("Work"), soon(), &|| true, &mut |_| {})
    });
    let stopped = tilo_test_env::within("a call stopped beforehand returns", stopped)
        .await
        .unwrap();
    assert_eq!(
        stopped.unwrap_err(),
        ModelRefusal::Provider(ModelError::new(ErrorKind::Cancelled))
    );
}

/// A plain question is answered in words, with no tools offered: what a
/// judgment, a label or a summary asks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_question_is_answered_in_words() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering([Reply::text("yes").costing(12)]);
    workspaces.answer_models_with(scripted.clone());
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let model = workspaces.workspace_model(ADA, Interaction::Classifier);

    let said = tokio::task::spawn_blocking(move || {
        model.say_blocking(
            ("Answer yes or no.", "Is this sender someone I know?"),
            16,
            soon(),
            &|| false,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(said.text(), "yes");
    assert_eq!(said.response.cost_micros, 12);
    let asked = &scripted.asked()[0].request;
    assert!(asked.tools.is_empty());
    assert_eq!(asked.tool_choice, ToolChoice::None);
    assert_eq!(asked.max_output_tokens, 16);
    assert_eq!(
        asked.messages,
        vec![
            Message::text(Role::System, "Answer yes or no."),
            Message::text(Role::User, "Is this sender someone I know?"),
        ]
    );
}

/// A picture goes only to a model that reads pictures: one that does not is
/// never asked, and members are told which model it is.
#[tokio::test]
async fn a_picture_goes_only_to_a_model_that_reads_pictures() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering([Reply::text("A receipt.")]);
    workspaces.answer_models_with(scripted.clone());
    let provider = workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let model = workspaces.workspace_model(ADA, Interaction::Request);
    let mut pictured = asking("What is this?");
    pictured.messages[0].content.push(Content::Image {
        media_type: "image/png".into(),
        base64: "AAAA".into(),
    });

    let refused = model
        .answer(&pictured, soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::CannotTake {
            model: "gemini-main".into(),
            what: Input::Picture,
        }
    );
    assert_eq!(refused.code(), "cannot-take");
    assert_eq!(
        refused.words(None),
        "gemini-main does not read pictures. Choose a model that does in Settings."
    );
    assert!(scripted.asked().is_empty(), "the model is not asked");

    // The provider's list says the model reads pictures: it is asked.
    let root = workspaces.workspace_root(&ADA.to_string());
    let mut config = provider_config(&root);
    for model in &mut config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == provider)
        .unwrap()
        .models
    {
        model.vision = true;
    }
    save_provider_config(&root, &config).unwrap();
    let answer = model.answer(&pictured, soon(), &mut |_| {}).await.unwrap();
    assert_eq!(answer.text(), "A receipt.");
    assert_eq!(scripted.asked()[0].request, pictured);
}

/// What a provider is handed prints without its key, and so does everything
/// the entry answers with.
#[tokio::test]
async fn nothing_printed_carries_the_key() {
    /// Keeps how each call it is handed prints.
    #[derive(Default)]
    struct Printing(Mutex<Vec<String>>);
    impl ModelProvider for Printing {
        fn answer<'a>(&'a self, call: ProviderCall<'a>, _: Events<'a>) -> ProviderFuture<'a> {
            assert_eq!(call.key(), Some(ADA_KEY), "the call holds the key");
            self.0.lock().unwrap().push(format!("{call:?}"));
            Box::pin(async { Err(ModelError::new(ErrorKind::RateLimited)) })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let printing = Arc::new(Printing::default());
    workspaces.answer_models_with(printing.clone());
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let model = workspaces.workspace_model(ADA, Interaction::Request);
    let refused = model
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap_err();

    let scripted = ScriptedProvider::answering([Reply::text("Hello")]);
    workspaces.answer_models_with(scripted.clone());
    let answer = model
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap();

    let mut printed = printing.0.lock().unwrap().clone();
    assert_eq!(printed.len(), 1);
    printed.extend([
        format!("{model:?}"),
        format!("{refused:?} {refused}"),
        format!("{answer:?}"),
        format!("{:?}", model.linked().unwrap()),
        format!("{:?}", scripted.asked()),
    ]);
    for line in &printed {
        assert!(!line.contains(ADA_KEY), "{line}");
    }
    assert!(printed[0].contains("keyed: true"), "{}", printed[0]);
}

fn scout(day: &str, limit_micros: Option<u64>) -> Spender {
    Spender {
        workspace: "a-workspace".into(),
        agent: "scout".into(),
        name: "Scout".into(),
        day: day.into(),
        limit_micros,
    }
}

/// An agent's day counts every answer, and the call that starts once its
/// agent's limit is reached is refused before the provider is asked: the
/// limit stops a run. A new day starts the count again, another agent has
/// its own day, and a host that restarts still knows the day.
#[tokio::test]
async fn the_days_limit_stops_a_run() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    // The device's keys stay its own across the restart, as its key service keeps them.
    let keys =
        std::sync::Arc::<tilo_workspace_service::workspaces::fixtures::TestGroupKeys>::default();
    let server = tilo_workspace_service::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &owner,
        keys.clone(),
    )
    .await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering(std::iter::repeat_n(
        Reply::text("a step").costing(1_200_000),
        4,
    ));
    workspaces.answer_models_with(scripted.clone());
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let today = scout("2026-11-17", Some(2_000_000));
    let model = workspaces
        .workspace_model(ADA, Interaction::Task)
        .spending_as(today.clone());

    let first = model.answer(&asking("Go on"), soon(), &mut |_| {}).await;
    assert_eq!(first.unwrap().spent_today_micros, Some(1_200_000));
    let second = model.answer(&asking("Go on"), soon(), &mut |_| {}).await;
    assert_eq!(second.unwrap().spent_today_micros, Some(2_400_000));
    let refused = model
        .answer(&asking("Go on"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::DayLimitReached {
            agent: "Scout".into(),
            limit_micros: 2_000_000,
            spent_micros: 2_400_000,
        }
    );
    assert_eq!(refused.code(), "day-limit");
    assert_eq!(
        refused.words(None),
        "Scout has used today's $2. Raise its limit to let it go on."
    );
    assert_eq!(scripted.asked().len(), 2, "the provider is not asked again");

    // Another agent of the same person has its own day.
    let other = Spender {
        agent: "second".into(),
        name: "Second".into(),
        ..today.clone()
    };
    let theirs = workspaces
        .workspace_model(ADA, Interaction::Task)
        .spending_as(other)
        .answer(&asking("Go on"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(theirs.spent_today_micros, Some(1_200_000));

    // The host restarts: the day is still spent.
    let server = {
        server.shutdown().await;
        tilo_workspace_service::workspaces::fixtures::test_host_holding(
            dir.path(),
            &key,
            &owner,
            keys,
        )
        .await
    };
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering([Reply::text("a new day").costing(10_000)]);
    workspaces.answer_models_with(scripted.clone());
    let model = workspaces.workspace_model(ADA, Interaction::Task);
    let still = model
        .clone()
        .spending_as(today)
        .answer(&asking("Go on"), soon(), &mut |_| {})
        .await;
    assert_eq!(still.unwrap_err().code(), "day-limit");
    assert!(scripted.asked().is_empty());

    // The next day starts at nothing.
    let tomorrow = model
        .spending_as(scout("2026-11-18", Some(2_000_000)))
        .answer(&asking("Go on"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(tomorrow.spent_today_micros, Some(10_000));
    assert_eq!(money(420_000), "$0.42");
}

/// A call may spend more than one day: its agent's, and that of a piece of
/// the agent's work its person bounded on its own. Each counts every
/// answer, the piece's own limit refuses its next call before the provider
/// is asked, and the agent's other work goes on under the agent's day.
#[tokio::test]
async fn a_piece_of_work_with_a_day_of_its_own_stops_at_its_own_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    let scripted = ScriptedProvider::answering(std::iter::repeat_n(
        Reply::text("a step").costing(400_000),
        3,
    ));
    workspaces.answer_models_with(scripted.clone());
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let agent = scout("2026-11-17", Some(1_200_000));
    let watching = Spender {
        agent: "event:watching".into(),
        name: "Watching the shelves".into(),
        limit_micros: Some(500_000),
        ..agent.clone()
    };
    let model = workspaces
        .workspace_model(ADA, Interaction::Task)
        .spending_as(agent.clone());
    let bounded = model.clone().spending_as(watching.clone());

    let first = bounded.answer(&asking("Look"), soon(), &mut |_| {}).await;
    assert_eq!(
        first.unwrap().spent_today_micros,
        Some(400_000),
        "the answer says the agent's day"
    );
    assert_eq!(bounded.spent_by(&watching), 400_000);
    let second = bounded.answer(&asking("Look"), soon(), &mut |_| {}).await;
    assert_eq!(second.unwrap().spent_today_micros, Some(800_000));
    assert_eq!(bounded.spent_by(&watching), 800_000);
    let refused = bounded
        .answer(&asking("Look"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::DayLimitReached {
            agent: "Watching the shelves".into(),
            limit_micros: 500_000,
            spent_micros: 800_000,
        }
    );
    assert_eq!(scripted.asked().len(), 2, "the provider is not asked again");

    // The agent's other work goes on under its own day, which counted the
    // piece's calls too, and spends nothing of the piece's.
    let other = model.answer(&asking("Go on"), soon(), &mut |_| {}).await;
    assert_eq!(other.unwrap().spent_today_micros, Some(1_200_000));
    assert_eq!(model.spent_by(&watching), 800_000);

    // The agent's day is spent now: it refuses another piece of its work
    // too, however much that piece has left.
    let fresh = Spender {
        agent: "event:fresh".into(),
        name: "Fresh".into(),
        limit_micros: Some(5_000_000),
        ..agent.clone()
    };
    let refused = model
        .clone()
        .spending_as(fresh.clone())
        .answer(&asking("Look"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, ModelRefusal::DayLimitReached { agent, .. } if agent == "Scout"),
        "{refused:?}"
    );
    assert_eq!(model.spent_by(&fresh), 0);
    assert_eq!(scripted.asked().len(), 3);
}

/// Dozens of calls at once for one agent are each counted: no answer's cost
/// is lost, whichever order they end in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_at_once_are_each_counted_on_the_day() {
    const CALLS: u64 = 36;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = crate::PersonServices::of(server.workspaces());
    workspaces.answer_models_with(ScriptedProvider::answering(
        (0..=CALLS).map(|_| Reply::text("a part").costing(1_000)),
    ));
    workspaces
        .link_model_for(&ADA.to_string(), &GOOGLE, true)
        .await
        .unwrap();
    let model = workspaces
        .workspace_model(ADA, Interaction::Subtask)
        .spending_as(scout("2026-11-17", None));

    let mut running = tokio::task::JoinSet::new();
    for _ in 0..CALLS {
        let model = model.clone();
        running.spawn(async move {
            model
                .answer(&asking("A part"), soon(), &mut |_| {})
                .await
                .unwrap()
                .spent_today_micros
                .unwrap()
        });
    }
    let mut totals = running.join_all().await;
    totals.sort_unstable();
    assert_eq!(
        totals,
        (1..=CALLS).map(|call| call * 1_000).collect::<Vec<_>>(),
        "each answer saw the day one answer further"
    );
    let after = model.answer(&asking("One more"), soon(), &mut |_| {}).await;
    assert_eq!(after.unwrap().spent_today_micros, Some((CALLS + 1) * 1_000));
}

/// A workspace's settings under `root`: one provider naming its key in the workspace's vault
/// and two models, the first chosen for each of `kinds`, the second the default where
/// `default` says so. Which model answers is read from the settings alone.
fn chose(root: &Path, kinds: &[Interaction], default: bool) {
    let mut config = provider_config(root);
    config.profiles.push(ProviderProfile {
        id: "made-up".into(),
        label: "Made-up models".into(),
        protocol: ProviderProtocol::OpenAiCompatible,
        endpoint: "https://models.example.com/v1".into(),
        models: vec![
            ModelDescriptor::new("made-up-main"),
            ModelDescriptor::new("made-up-default"),
        ],
        default_model: default.then(|| "made-up-default".to_owned()),
        credential: Some(agent_provider_config::SecretReference::workspace_vault(
            "vault.item:0f1e2d3c4b5a69788796a5b4c3d2e1f0",
        )),
        ..ProviderProfile::default()
    });
    if default {
        config.default_profile = Some("made-up".into());
    }
    for kind in kinds {
        let chosen = agent_provider_config::AgentModel {
            profile: "made-up".into(),
            model: "made-up-main".into(),
        };
        config.set_interaction_model(*kind, Some(chosen)).unwrap();
    }
    save_provider_config(root, &config).unwrap();
}

/// The model `kind` of work is answered by under `root`, or why by none.
fn answered_by(root: &Path, kind: Interaction) -> Result<String, ModelRefusal> {
    resolve(root, (kind, None), false, &NoProvider).map(|resolved| resolved.model.id)
}

/// A workspace's main thread is one choice kept as two kinds: what answers
/// there and what works there. Chosen for one of them only, as a device that
/// chooses them with two asks leaves it between the two, and with no
/// default, that model answers the other kind as well. No other kind of work
/// is answered by it, the main thread is answered by no other work's model,
/// and a default answers before the other kind does. A sub-agent has its own
/// rule ([`a_sub_agent_with_no_model_of_its_own_works_as_the_task_model`]).
#[test]
fn one_kind_of_the_main_thread_answers_for_its_other_kind_and_for_no_other_work() {
    const OTHER_WORK: [Interaction; 3] = [
        Interaction::OneShot,
        Interaction::Classifier,
        Interaction::Dictation,
    ];
    let main = "made-up-main".to_owned();
    for (chosen, other) in [
        (Interaction::Request, Interaction::Task),
        (Interaction::Task, Interaction::Request),
    ] {
        let dir = tempfile::tempdir().unwrap();
        chose(dir.path(), &[chosen], false);
        assert_eq!(answered_by(dir.path(), chosen), Ok(main.clone()));
        assert_eq!(
            answered_by(dir.path(), other),
            Ok(main.clone()),
            "chosen for {chosen}, {other} is answered by it"
        );
        for kind in OTHER_WORK {
            assert_eq!(
                answered_by(dir.path(), kind),
                Err(ModelRefusal::NoModelLinked { kind }),
                "chosen for {chosen}"
            );
        }
    }

    // A model chosen for other work is not the main thread's.
    let dir = tempfile::tempdir().unwrap();
    chose(dir.path(), &[Interaction::Subtask], false);
    for kind in [Interaction::Request, Interaction::Task] {
        assert_eq!(
            answered_by(dir.path(), kind),
            Err(ModelRefusal::NoModelLinked { kind })
        );
    }

    // A workspace with a default is answered as before: its default answers
    // the kind it chose nothing for.
    let dir = tempfile::tempdir().unwrap();
    chose(dir.path(), &[Interaction::Request], true);
    assert_eq!(answered_by(dir.path(), Interaction::Request), Ok(main));
    assert_eq!(
        answered_by(dir.path(), Interaction::Task),
        Ok("made-up-default".to_owned())
    );
}

/// A sub-agent a person's agent starts names no model of its own when the
/// skill it works does not: it is a piece of the person's task work and the
/// task model answers it. A model chosen only for what answers on the main
/// thread is not the task, and does not answer a sub-agent.
#[test]
fn a_sub_agent_with_no_model_of_its_own_works_as_the_task_model() {
    let dir = tempfile::tempdir().unwrap();
    chose(dir.path(), &[Interaction::Task], false);
    assert_eq!(
        answered_by(dir.path(), Interaction::Subtask),
        Ok("made-up-main".to_owned())
    );

    let dir = tempfile::tempdir().unwrap();
    chose(dir.path(), &[Interaction::Request], false);
    assert_eq!(
        answered_by(dir.path(), Interaction::Subtask),
        Err(ModelRefusal::NoModelLinked {
            kind: Interaction::Subtask,
        })
    );
}
