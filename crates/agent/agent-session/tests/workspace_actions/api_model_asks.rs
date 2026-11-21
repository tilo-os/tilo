//! A computer's apps ask the workspace's own model where a page asks it: on the server that keeps
//! the workspace, with the key its vault keeps there. The way there is the session the
//! computer holds with that server, the one its `tilo` commands use; the model gateway asks
//! through the API that session's holder serves (`POST …/model/ask`), and the answer comes
//! back as the model writes it. With nothing serving that API, the app is told there is no
//! model while offline.

use super::api::ISSUER;
use super::api_on_server::api_on_server;
use super::api_v1::{Holder, own_workspace};
use super::*;
use tilo_model_gateway::host_backend::protocol::{
    Content, ErrorKind, Message, ModelError, ModelRequest, ModelResponse, Role, ToolChoice,
};
use tilo_server::workspaces::person_model::{
    ProviderProtocol,
    scripted::{Linking, Reply, ScriptedProvider},
};

fn asking(text: &str) -> ModelRequest {
    ModelRequest {
        messages: vec![Message::text(Role::User, text)],
        tools: Vec::new(),
        tool_choice: ToolChoice::None,
        max_output_tokens: 256,
    }
}

/// A turn of the gateway's, through `client`: the words it was told as they came, and the
/// whole turn.
async fn ask(
    client: &tilo_api::client::Client,
    text: &str,
) -> (Vec<String>, Result<ModelResponse, ModelError>) {
    let mut words = Vec::new();
    let answer = tilo_model_gateway::service::server_asks::ask_with(
        client,
        &asking(text),
        tilo_test_env::hang_bound(),
        &mut |frame| {
            assert_eq!(frame.event.as_deref(), Some("text_delta"), "{frame:?}");
            let word: String = serde_json::from_str(frame.output.as_deref().unwrap()).unwrap();
            words.push(word);
            true
        },
    )
    .await;
    (words, answer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computers_model_asks_stream_from_its_workspaces_server_over_its_held_session() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let workspace = own_workspace(&on_server, &ada, &page).await;
    let workspaces = on_server.hosted.server.workspaces().unwrap().clone();
    let model = ScriptedProvider::answering([
        Reply::text("the launch is on Tuesday").pausing(Duration::from_millis(150))
    ]);
    workspaces.answer_models_with(model.clone());
    workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-ada-kept-on-her-server"),
                models: &["gemini-main"],
            },
            true,
        )
        .unwrap();
    let holder = Holder::start(&on_server, &ada, &workspace, "Claude Code").await;

    let (words, answer) = ask(&holder.command(), "when is the launch?").await;
    let answer = answer.expect("the workspace's model answers");
    assert_eq!(
        answer.message.content,
        [Content::Text {
            text: "the launch is on Tuesday".into()
        }]
    );
    assert!(
        words.len() > 1,
        "the words come as the model writes them: {words:?}"
    );
    assert_eq!(words.concat(), "the launch is on Tuesday");
    let asked = model.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].key.as_deref(),
        Some("sk-ada-kept-on-her-server"),
        "the server asks with the key it keeps"
    );
    assert_eq!(asked[0].model, "gemini-main");
    assert!(holder.device.is_live());

    // The holder gone, nothing on this computer answers for the server.
    let gone = tempfile::tempdir().unwrap();
    let nobody = tilo_api::client::Client::session(&gone.path().join("api.sock"));
    let (words, refused) = ask(&nobody, "and now?").await;
    assert!(words.is_empty());
    let refused = refused.expect_err("no model while offline");
    assert_eq!(refused.kind, ErrorKind::Offline);
    assert!(
        refused.message.starts_with("No model while offline"),
        "{}",
        refused.message
    );
    assert_eq!(model.asked().len(), 1, "nothing else was asked");
}
