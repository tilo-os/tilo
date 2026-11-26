//! Signing in with an `OpenAI` account from a page, on the real path: the
//! page's actions on the server, the server's device-code sign-in against a fake
//! `OpenAI`, the grant in the workspace's vault, and its model asked on the
//! plan through the model gateway, refreshed when it expires.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the server it drives"
)]

use std::sync::Arc;

use agent_provider_config::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, ToolChoice,
};
use fake_model_providers::FakeProviders;
use serde_json::{Value, json};
use tilo_actions::remote::person_models::{SignInStarted, SignInState};
use tilo_actions::remote::{ActionCallError, Caller, person_models};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::DeviceCert;

use super::ServiceWorkspaceActions;
use super::tests::{admit, ask_own, governed, request, settled};
use crate::workspaces::tests::{governor, test_host};
use crate::workspaces::{AsksPeople, OpenAiSignIn, PersonAnswer};

fn asking(text: &str) -> Value {
    serde_json::to_value(ModelRequest {
        messages: vec![Message::text(Role::User, text)],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 256,
    })
    .unwrap()
}

/// The words of the person's model's answer to `text`.
async fn answer(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    text: &str,
) -> Result<String, ModelError> {
    let answered = ask_own(
        actions,
        workspace_id,
        person,
        person_models::ASK,
        asking(text),
    )
    .await
    .unwrap();
    let answered: Result<ModelResponse, ModelError> = serde_json::from_value(answered).unwrap();
    Ok(answered?
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect())
}

/// Sign `person` in as `using` says, the code entered on the fake's page:
/// how the sign-in ended.
async fn signs_in(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    fake: &FakeProviders,
    using: Value,
) -> SignInState {
    let started = begin_sign_in(actions, workspace_id, person, using).await;
    assert_eq!(started.page, format!("{}/codex/device", fake.issuer_url()));
    assert!(
        fake.approve(&started.code),
        "the fake showed {}",
        started.code
    );
    wait_for_sign_in(actions, workspace_id, person, &started).await
}

async fn begin_sign_in(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    using: Value,
) -> SignInStarted {
    serde_json::from_value(
        ask_own(
            actions,
            workspace_id,
            person,
            person_models::BEGIN_SIGN_IN,
            using,
        )
        .await
        .expect("the sign-in begins"),
    )
    .unwrap()
}

async fn wait_for_sign_in(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    started: &SignInStarted,
) -> SignInState {
    for _ in 0..10 {
        let state = ask_own(
            actions,
            workspace_id,
            person,
            person_models::WAIT_SIGN_IN,
            json!({"signIn": started.sign_in}),
        )
        .await
        .expect("the sign-in is waited for");
        let state: SignInState = serde_json::from_value(state).unwrap();
        if state != SignInState::Waiting {
            return state;
        }
    }
    panic!("the sign-in never ended");
}

fn at_fake(fake: &FakeProviders) -> OpenAiSignIn {
    OpenAiSignIn {
        issuer: fake.issuer_url(),
        backend: fake.chatgpt_backend_url(),
        api: fake.openai_api_url(),
        ..OpenAiSignIn::default()
    }
}

/// A person who answers every question one way, or not at all.
struct Asked {
    answer: Option<ConfirmationAnswer>,
}

impl Asked {
    fn answering(answer: Option<ConfirmationAnswer>) -> Arc<Self> {
        Arc::new(Self { answer })
    }
}

impl AsksPeople for Asked {
    fn ask(&self, _: [u8; 32], _: PendingConfirmation) -> PersonAnswer {
        let answer = self
            .answer
            .clone()
            .ok_or_else(|| "nobody answered".to_owned());
        Box::pin(async move { answer })
    }
}

/// One of the sign-in actions asked by `caller` on `member`'s device.
async fn a_sign_in_asked_by(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    member: &DeviceCert,
    caller: &Caller,
    verb: &str,
) -> Result<(), ActionCallError> {
    let mut asking = request("", "", json!({"signIn": "any"}));
    asking.target = person_models::target(verb);
    settled(tilo_actions::remote::ActionProvider::invoke(
        &actions.in_workspace_as(workspace_id, member),
        caller,
        asking,
    ))
    .await
    .map(|_| ())
}

/// No agent signs in on its own: beginning or stopping a sign-in is put to its member first, and
/// one no device of theirs stands behind is answered nothing.
async fn no_agent_signs_in_on_its_own(
    workspaces: &crate::workspaces::Workspaces,
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    ada: &DeviceCert,
) {
    let assistant = super::tests::caller(ada).delegated_to("assistant");
    for verb in [person_models::BEGIN_SIGN_IN, person_models::CANCEL_SIGN_IN] {
        let unasked = a_sign_in_asked_by(actions, workspace_id, ada, &assistant, verb).await;
        assert!(
            matches!(unasked, Err(ActionCallError::ConfirmationRequired { .. })),
            "{verb} with nobody to ask through: {unasked:?}"
        );
    }
    let refusing = Asked::answering(Some(ConfirmationAnswer::Deny));
    workspaces.ask_people(refusing);
    for verb in [person_models::BEGIN_SIGN_IN, person_models::CANCEL_SIGN_IN] {
        let refused = a_sign_in_asked_by(actions, workspace_id, ada, &assistant, verb).await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{verb} its member refused: {refused:?}"
        );
    }
    let person = hex::encode(ada.user_public_key);
    let nobodies = [
        Caller::agent_turn(
            "assistant",
            person.clone(),
            tilo_action_policy::turn::Turn::new("chat/main", "message-1"),
        ),
        Caller::local_agent("assistant"),
        Caller::local_app("so.tilo.chat").acting_for(person),
    ];
    for nobody in &nobodies {
        for verb in [
            person_models::BEGIN_SIGN_IN,
            person_models::WAIT_SIGN_IN,
            person_models::CANCEL_SIGN_IN,
        ] {
            let refused = a_sign_in_asked_by(actions, workspace_id, ada, nobody, verb).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{verb} as {:?}: {refused:?}",
                nobody.acting()
            );
        }
    }
}

/// A member signs in to their `ChatGPT` plan from a page, entering a code on
/// `OpenAI`'s page, with no key typed and no device of theirs about. The
/// plan's models are listed and the first becomes the workspace's default; a
/// turn is answered on it with the grant its vault keeps, refreshed when it
/// expires, and nothing a page reads carries a token. No agent signs in on
/// its own.
#[tokio::test]
async fn a_member_signs_in_to_chatgpt_from_a_page_and_the_workspaces_model_answers_on_the_plan() {
    let fake = FakeProviders::start().await.unwrap();
    // Access tokens expired at once: every ask finds one to refresh.
    fake.set_token_lifetime(1);
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    workspaces.sign_in_to_openai_at(at_fake(&fake));
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    let SignInState::Done { provider } =
        signs_in(&actions, workspace_id, &ada, &fake, json!({})).await
    else {
        panic!("the sign-in did not finish");
    };
    assert!(provider.signs_in && provider.configured, "{provider:?}");
    assert_eq!(provider.endpoint, fake.chatgpt_backend_url());
    assert!(
        provider.models.iter().any(|model| model.id == "gpt-5.5"),
        "{provider:?}"
    );
    let listed = ask_own(&actions, workspace_id, &ada, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(listed["defaultProvider"], json!(provider.id), "{listed}");
    // A JSON web token begins with an encoded `{"`: none is in what a page reads.
    assert!(!listed.to_string().contains("eyJ"), "{listed}");

    let first = answer(&actions, workspace_id, &ada, "Hello").await.unwrap();
    assert!(first.starts_with("[ChatGPT"), "{first}");
    let refreshed = fake.refresh_count();
    assert!(refreshed >= 1, "a token that expired was refreshed");
    // The refreshed grant was kept: the next ask refreshes the newest one.
    let second = answer(&actions, workspace_id, &ada, "Again").await.unwrap();
    assert!(second.starts_with("[ChatGPT"), "{second}");
    assert!(fake.refresh_count() > refreshed);

    // A key is not what a plan is reached with.
    let keyed = settled_own(
        &actions,
        workspace_id,
        &ada,
        person_models::SET_KEY,
        json!({"provider": provider.id, "key": "sk-typed"}),
    )
    .await;
    assert!(
        matches!(keyed, Err(ActionCallError::Refused { .. })),
        "{keyed:?}"
    );

    no_agent_signs_in_on_its_own(workspaces, &actions, workspace_id, &ada).await;

    // Signing out forgets the grant: the plan is not asked, and the person
    // is told to sign in again.
    ask_own(
        &actions,
        workspace_id,
        &ada,
        person_models::REMOVE_KEY,
        json!({"provider": provider.id}),
    )
    .await
    .unwrap();
    let signed_out = answer(&actions, workspace_id, &ada, "Still there?")
        .await
        .unwrap_err();
    assert!(
        signed_out.message.contains("sign in again"),
        "{signed_out:?}"
    );
    server.shutdown().await;
}

/// A signed-in provider rejecting its access token can trigger another model
/// call. The original turn's permission must still hold for that retry.
#[tokio::test]
async fn a_signed_in_model_rechecks_the_turns_permission_after_authentication_fails() {
    use crate::workspaces::person_model::scripted::{Reply, ScriptedProvider};
    use crate::workspaces::person_model::{Interaction, protocol::ErrorKind};

    let fake = FakeProviders::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    workspaces.sign_in_to_openai_at(at_fake(&fake));
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let signed_in = signs_in(&actions, workspace_id, &ada, &fake, json!({})).await;
    assert!(
        matches!(signed_in, SignInState::Done { .. }),
        "{signed_in:?}"
    );
    let refreshes = fake.refresh_count();

    let provider = ScriptedProvider::answering([
        Reply::failing(ModelError::new(ErrorKind::Authentication)),
        Reply::text("The person asked directly."),
    ]);
    workspaces.answer_models_with(provider.clone());
    let receipt = Arc::clone(&provider);
    let turn = workspaces
        .workspace_model(workspace_id, Interaction::Request)
        .while_permitted(Arc::new(move || {
            Box::pin(std::future::ready(receipt.asked().is_empty()))
        }));
    let request = serde_json::from_value(asking("Private turn")).unwrap();
    let deadline = tilo_clock::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap();
    let refused = turn
        .answer(&request, deadline, &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(refused.model_error().kind, ErrorKind::Cancelled);
    assert_eq!(
        provider.asked().len(),
        1,
        "no retry sent the private prompt"
    );
    assert_eq!(
        fake.refresh_count(),
        refreshes,
        "the withdrawn turn did not refresh the token"
    );
    assert_eq!(
        answer(&actions, workspace_id, &ada, "My own question")
            .await
            .unwrap(),
        "The person asked directly."
    );
    server.shutdown().await;
}

/// Signing in for API usage has `OpenAI` issue an API key for the account,
/// kept as the `OpenAI` provider's key; an account with no API organisation
/// is told what to do instead.
#[tokio::test]
async fn signing_in_for_api_usage_keeps_an_issued_key() {
    let fake = FakeProviders::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    workspaces.sign_in_to_openai_at(at_fake(&fake));
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    fake.set_api_key_exchange(false);
    let SignInState::Failed { reason } = signs_in(
        &actions,
        workspace_id,
        &ada,
        &fake,
        json!({"using": "api-usage"}),
    )
    .await
    else {
        panic!("an account with no API organisation is not signed in");
    };
    assert!(reason.contains("API organisation"), "{reason}");

    fake.set_api_key_exchange(true);
    let SignInState::Done { provider } = signs_in(
        &actions,
        workspace_id,
        &ada,
        &fake,
        json!({"using": "api-usage"}),
    )
    .await
    else {
        panic!("the sign-in did not finish");
    };
    assert!(!provider.signs_in && provider.configured, "{provider:?}");
    assert_eq!(provider.endpoint, fake.openai_api_url());
    let answered = answer(&actions, workspace_id, &ada, "Hello").await.unwrap();
    assert!(answered.starts_with("[OpenAI"), "{answered}");
    server.shutdown().await;
}

#[tokio::test]
async fn a_provider_sign_in_cannot_persist_after_the_workspace_revokes_its_action() {
    use crate::workspaces::workspace_stores::Store;
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet};

    let fake = FakeProviders::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    workspaces.sign_in_to_openai_at(at_fake(&fake));
    let (workspace, id, actions) = governed(workspaces).await;
    let person = admit(workspaces, &workspace, &owner, 1).await;
    let started = begin_sign_in(&actions, id, &person, json!({})).await;
    let deny = Grant::new(
        "deny-provider-sign-in",
        Grantee::person(hex::encode(person.user_public_key)),
        Store::MODELS.node(&workspace, person_models::BEGIN_SIGN_IN),
        KindSet::UNDOABLE,
        Answer::Deny,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    workspaces
        .put_workspace_grants(&workspace, &owner, [3; 32], vec![deny])
        .await
        .unwrap();
    assert!(fake.approve(&started.code));
    assert!(matches!(
        wait_for_sign_in(&actions, id, &person, &started).await,
        SignInState::Failed { .. }
    ));
    let providers = ask_own(&actions, id, &owner, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(providers["providers"], json!([]));
    server.shutdown().await;
}

#[tokio::test]
async fn one_persons_provider_sign_ins_in_two_workspaces_are_independent() {
    let fake = FakeProviders::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    workspaces.sign_in_to_openai_at(at_fake(&fake));
    let (_, first, actions) = governed(workspaces).await;
    let (_, second, _) = governed(workspaces).await;
    let a = begin_sign_in(&actions, first, &owner, json!({})).await;
    let b = begin_sign_in(&actions, second, &owner, json!({})).await;
    let other_attempt = json!({"signIn": a.sign_in});
    let wrong = settled_own(
        &actions,
        second,
        &owner,
        person_models::WAIT_SIGN_IN,
        other_attempt.clone(),
    )
    .await;
    assert!(
        matches!(wrong, Err(ActionCallError::Refused { .. })),
        "{wrong:?}"
    );
    ask_own(
        &actions,
        second,
        &owner,
        person_models::CANCEL_SIGN_IN,
        other_attempt,
    )
    .await
    .unwrap();
    for (workspace, started) in [(first, a), (second, b)] {
        assert!(fake.approve(&started.code));
        assert!(matches!(
            wait_for_sign_in(&actions, workspace, &owner, &started).await,
            SignInState::Done { .. }
        ));
    }
    server.shutdown().await;
}

/// `verb` of `person`'s own, as a refusal or its answer.
async fn settled_own(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    verb: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let mut asking = request("", "", argument);
    asking.target = person_models::target(verb);
    let receipt = settled(tilo_actions::remote::ActionProvider::invoke(
        &actions.in_workspace_as(workspace_id, person),
        &super::tests::caller(person),
        asking,
    ))
    .await?;
    match receipt.status {
        tilo_actions::remote::ReceiptStatus::Completed { output } => {
            Ok(output.value.unwrap_or(Value::Null))
        }
        other => panic!("{other:?}"),
    }
}
