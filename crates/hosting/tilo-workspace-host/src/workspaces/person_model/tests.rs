//! A person's model, from their store to the provider's answer: which model
//! a kind of work runs on, every reason nothing answers, the stream, the
//! cost, and a call that is stopped.

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

use super::scripted::{Linking, Reply, ScriptedProvider};
use super::*;
use crate::workspaces::person_settings::save_provider_config;
use crate::workspaces::tests::{governor, test_host};

const ADA: [u8; 32] = [7; 32];
const BO: [u8; 32] = [9; 32];
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

/// A person who linked nothing is refused in words they can act on, the
/// provider is never asked, and asking keeps no store for them.
#[tokio::test]
async fn nothing_linked_is_refused_and_asks_no_provider() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let scripted = ScriptedProvider::answering([Reply::text("never said")]);
    workspaces.answer_models_with(scripted.clone());

    let model = workspaces.person_model(ADA, Interaction::Task);
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
        "No model is linked for this kind of work. Link one in Settings."
    );
    assert_eq!(
        refused.words(Some("de-AT")),
        "Für diese Art von Arbeit ist kein Modell verknüpft. Verknüpfe eines in den Einstellungen."
    );
    assert_eq!(refused.model_error().kind, ErrorKind::NoProvider);

    let answered = model.answer(&asking("Hello"), soon(), &mut |_| {}).await;
    assert_eq!(answered.unwrap_err(), refused);
    assert!(scripted.asked().is_empty(), "no provider is asked");
    assert!(
        !workspaces.person_settings_root(&ADA).exists(),
        "asking keeps no store for a person who set nothing up"
    );
}

/// A kind of work runs on the model chosen for it and every other kind on
/// the person's default, each asked with its own provider's key; another
/// person on the same host is answered by their own.
#[tokio::test]
async fn a_kind_runs_on_its_own_model_and_the_rest_on_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let scripted = ScriptedProvider::answering([
        Reply::text("main thread"),
        Reply::text("a part"),
        Reply::text("for Bo"),
    ]);
    workspaces.answer_models_with(scripted.clone());

    workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();
    let parts = workspaces.link_model_for(&ADA, &ANTHROPIC, false).unwrap();
    workspaces
        .choose_model_for(&ADA, Interaction::Subtask, &parts, "claude-parts")
        .unwrap();
    let bo = Linking {
        name: "Bo's own",
        endpoint: "https://models.example.com/v1",
        key: Some(BO_KEY),
        models: &["open-model"],
        ..GOOGLE
    };
    workspaces.link_model_for(&BO, &bo, true).unwrap();

    let main = workspaces
        .person_model(ADA, Interaction::Request)
        .answer(&asking("Good morning"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(text_of(&main), "main thread");
    assert_eq!(main.linked.provider, "Google");
    assert_eq!(main.linked.model.id, "gemini-main");
    let part = workspaces
        .person_model(ADA, Interaction::Subtask)
        .answer(&asking("Look this up"), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(part.linked.provider, "Anthropic");
    assert_eq!(part.linked.protocol, ProviderProtocol::Anthropic);
    let theirs = workspaces
        .person_model(BO, Interaction::Request)
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
/// names it: the person's default does not answer in its place.
#[tokio::test]
async fn a_choice_that_cannot_be_used_is_refused_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let scripted = ScriptedProvider::answering([Reply::text("the default")]);
    workspaces.answer_models_with(scripted.clone());
    workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();
    let parts = workspaces.link_model_for(&ADA, &ANTHROPIC, false).unwrap();
    workspaces
        .choose_model_for(&ADA, Interaction::Task, &parts, "claude-parts")
        .unwrap();

    // The person switches the provider off.
    let root = workspaces.person_settings_root(&ADA);
    let mut config = provider_config(&root);
    config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == parts)
        .unwrap()
        .enabled = false;
    save_provider_config(&root, &config).unwrap();

    let refused = workspaces
        .person_model(ADA, Interaction::Task)
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
            .person_model(ADA, Interaction::Request)
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
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let keyless = Linking {
        key: None,
        ..GOOGLE
    };
    workspaces.link_model_for(&ADA, &keyless, true).unwrap();

    let refused = workspaces
        .person_model(ADA, Interaction::Request)
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
/// called only on a host whose operator allows it: a person's settings never
/// point the host at its own network.
#[tokio::test]
async fn an_address_off_the_public_internet_needs_the_operators_word() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    for (person, endpoint) in [
        ([1; 32], "http://127.0.0.1:8000/v1"),
        ([2; 32], "https://10.0.0.5/v1"),
        ([3; 32], "https://models.lan/v1"),
        ([4; 32], "http://models.example.com/v1"),
        ([5; 32], "https://localhost/v1"),
    ] {
        let local = Linking {
            name: "A model server",
            endpoint,
            key: None,
            ..GOOGLE
        };
        workspaces.link_model_for(&person, &local, true).unwrap();
        let model = workspaces.person_model(person, Interaction::Request);
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
            .person_model([1; 32], Interaction::Request)
            .linked()
            .unwrap_err()
            .model_error()
            .kind,
        ErrorKind::PermissionDenied
    );

    // The operator's own network, and a server there that takes no key.
    workspaces.call_private_model_addresses(true);
    let linked = workspaces
        .person_model([1; 32], Interaction::Request)
        .linked()
        .unwrap();
    assert_eq!(linked.provider, "A model server");
}

/// The answer arrives as it streams and whole at its end, with what it
/// cost.
#[tokio::test]
async fn the_answer_streams_and_says_what_it_cost() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let scripted = ScriptedProvider::answering([
        Reply::text("Good morning, Ada. Two things wait for you.").costing(1_250),
        Reply::tool_call("call-1", "invoke", serde_json::json!({"action": "post"})).costing(90),
    ]);
    workspaces.answer_models_with(scripted);
    workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();
    let model = workspaces.person_model(ADA, Interaction::Request);

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
/// show.
#[tokio::test]
async fn a_providers_failure_is_the_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces.answer_models_with(ScriptedProvider::answering([Reply::failing(
        ModelError::new(ErrorKind::Authentication),
    )]));
    workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();

    let refused = workspaces
        .person_model(ADA, Interaction::Request)
        .answer(&asking("Hello"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "provider");
    assert_eq!(
        refused,
        ModelRefusal::Provider(ModelError::new(ErrorKind::Authentication))
    );
    assert_eq!(
        refused.words(None),
        "Provider authentication failed; check the credential in Settings"
    );
}

/// A host that was given nothing to speak to providers with answers no
/// call, and never with a model of somebody else's.
#[tokio::test]
async fn a_host_that_speaks_to_no_provider_answers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces.answer_models_with(Arc::new(NoProvider));
    workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();
    let refused = workspaces
        .person_model(ADA, Interaction::Request)
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
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let scripted = ScriptedProvider::answering([Reply::never(), Reply::never(), Reply::never()]);
    workspaces.answer_models_with(scripted.clone());
    workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();
    let model = workspaces.person_model(ADA, Interaction::Task);

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
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let scripted = ScriptedProvider::answering([Reply::text("yes").costing(12)]);
    workspaces.answer_models_with(scripted.clone());
    workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();
    let model = workspaces.person_model(ADA, Interaction::Classifier);

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
/// never asked, and the person is told which model it is.
#[tokio::test]
async fn a_picture_goes_only_to_a_model_that_reads_pictures() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let scripted = ScriptedProvider::answering([Reply::text("A receipt.")]);
    workspaces.answer_models_with(scripted.clone());
    let provider = workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();
    let model = workspaces
        .person_model(ADA, Interaction::Request)
        .working_in(tilo_relay_protocol::WorkspaceId::default());
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
    let root = workspaces.person_settings_root(&ADA);
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
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let printing = Arc::new(Printing::default());
    workspaces.answer_models_with(printing.clone());
    workspaces.link_model_for(&ADA, &GOOGLE, true).unwrap();
    let model = workspaces.person_model(ADA, Interaction::Request);
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
