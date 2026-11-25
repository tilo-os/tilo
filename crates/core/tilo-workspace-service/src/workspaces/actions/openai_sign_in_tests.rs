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

use agent_provider_config::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, ToolChoice,
};
use fake_model_providers::FakeProviders;
use serde_json::{Value, json};
use tilo_actions::remote::model_providers::{SignInStarted, SignInState};
use tilo_actions::remote::{ActionCallError, Caller, model_providers};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::ServiceWorkspaceActions;
use super::tests::{Answering, admit, answered, ask_own, hosted, request, settled};
use crate::workspaces::OpenAiSignIn;
use crate::workspaces::tests::{hosting_device, test_host};

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
        model_providers::ASK,
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
    let started = ask_own(
        actions,
        workspace_id,
        person,
        model_providers::BEGIN_SIGN_IN,
        using,
    )
    .await
    .expect("the sign-in begins");
    let started: SignInStarted = serde_json::from_value(started).unwrap();
    assert_eq!(started.page, format!("{}/codex/device", fake.issuer_url()));
    assert!(
        fake.approve(&started.code),
        "the fake showed {}",
        started.code
    );
    for _ in 0..10 {
        let state = ask_own(
            actions,
            workspace_id,
            person,
            model_providers::WAIT_SIGN_IN,
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

/// One of the sign-in actions asked by `caller` on `member`'s device.
async fn a_sign_in_asked_by(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    member: &DeviceCert,
    caller: &Caller,
    verb: &str,
) -> Result<(), ActionCallError> {
    let mut asking = request("", "", json!({"signIn": "any"}));
    asking.target = model_providers::target(verb);
    let first = settled(tilo_actions::remote::ActionProvider::invoke(
        &actions.device_page(workspace_id, member),
        caller,
        asking.clone(),
    ))
    .await;
    let workspaces = actions
        .shared
        .workspaces
        .get()
        .and_then(crate::workspaces::WeakWorkspaces::upgrade)
        .unwrap();
    answered(
        &workspaces,
        (Some(workspace_id), Some(member)),
        caller,
        &asking,
        first,
    )
    .await
    .map(|_| ())
}

/// No agent signs in on its own: beginning or stopping a sign-in is put to its member first, and
/// one no device of theirs stands behind is answered nothing.
async fn no_agent_signs_in_on_its_own(
    _workspaces: &crate::workspaces::Workspaces,
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    ada: &DeviceCert,
) {
    let assistant = super::tests::caller(ada).delegated_to("assistant");
    for verb in [
        model_providers::BEGIN_SIGN_IN,
        model_providers::CANCEL_SIGN_IN,
    ] {
        let unasked = a_sign_in_asked_by(actions, workspace_id, ada, &assistant, verb).await;
        assert!(
            matches!(unasked, Err(ActionCallError::ConfirmationRequired { .. })),
            "{verb} waits for its member's answer: {unasked:?}"
        );
    }
    let refusing = Answering::declining();
    for verb in [
        model_providers::BEGIN_SIGN_IN,
        model_providers::CANCEL_SIGN_IN,
    ] {
        let refused = a_sign_in_asked_by(actions, workspace_id, ada, &assistant, verb).await;
        assert!(
            matches!(refused, Err(ActionCallError::ConfirmationRequired { .. })),
            "{verb} its member declined: {refused:?}"
        );
    }
    assert_eq!(refusing.asked().len(), 2, "each asked its member");
    let person = hex::encode(ada.user_public_key);
    let nobodies = [
        Caller::local_agent("assistant", "ada"),
        Caller::local_app("so.tilo.chat").acting_for(person),
    ];
    for nobody in &nobodies {
        for verb in [
            model_providers::BEGIN_SIGN_IN,
            model_providers::WAIT_SIGN_IN,
            model_providers::CANCEL_SIGN_IN,
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    workspaces.sign_in_to_openai_at(at_fake(&fake));
    let (workspace, workspace_id, actions) = hosted(workspaces).await;
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
    let listed = ask_own(
        &actions,
        workspace_id,
        &ada,
        model_providers::LIST,
        json!({}),
    )
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
        model_providers::SET_KEY,
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
        model_providers::REMOVE_KEY,
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

/// Signing in for API usage has `OpenAI` issue an API key for the account,
/// kept as the `OpenAI` provider's key; an account with no API organisation
/// is told what to do instead.
#[tokio::test]
async fn signing_in_for_api_usage_keeps_an_issued_key() {
    let fake = FakeProviders::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    workspaces.sign_in_to_openai_at(at_fake(&fake));
    let (workspace, workspace_id, actions) = hosted(workspaces).await;
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

/// `verb` of `person`'s own, as a refusal or its answer.
async fn settled_own(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    verb: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let mut asking = request("", "", argument);
    asking.target = model_providers::target(verb);
    let receipt = settled(tilo_actions::remote::ActionProvider::invoke(
        &actions.device_page(workspace_id, person),
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
