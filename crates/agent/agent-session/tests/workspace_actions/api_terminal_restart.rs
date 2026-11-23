//! A workspace a server made for a person, and handed to them, runs their
//! key's commands in its Terminal after the server restarts as it did before:
//! the server serves it as one of its administrators, and a restart must not
//! leave it unable to read or write the workspace's Terminal session.

use super::api::ISSUER;
use super::api_on_server::{api_on_server_shelled, approved_key};
use super::api_unattended::{Key, answer, backend, page_in, waiting, workspace};
use super::*;
use serde_json::Value;

/// Whether `answer` is the output of a command that printed `printed`.
fn printed(status: u16, answer: &Value, printed: &str) -> bool {
    status == 200 && answer.to_string().contains(printed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handed_workspace_runs_its_keys_commands_after_the_server_restarts() {
    Box::pin(runs_after_a_restart()).await;
}

async fn runs_after_a_restart() {
    let on_server = Box::pin(api_on_server_shelled(ISSUER)).await;
    let (ada, token) = backend(&on_server).await;
    let ws = {
        let backend = Key {
            api: &on_server.api,
            token: token.clone(),
        };
        workspace(&backend, "Ada's").await
    };
    let minted = Key {
        api: &on_server.api,
        token: token.clone(),
    }
    .mint(&ws, "Ada's agent")
    .await;

    // Ada lets the workspace's own key run commands unattended, on her page.
    {
        let agent = Key {
            api: &on_server.api,
            token: minted.clone(),
        };
        let mut person = page_in(&on_server, &ada, &ws).await;
        let (status, asking) = agent.unattended(&ws, true).await;
        let run = waiting(status, &asking).unwrap_or_else(|| panic!("pending: {status} {asking}"));
        answer(&mut person, &run, "approve").await;
        let (status, allowed) = agent.follow(&ws, &run).await;
        assert_eq!(status, 200, "{allowed}");
        let (status, ran) = agent.run(&ws, "echo before-restart").await;
        assert!(
            printed(status, &ran, "before-restart"),
            "ran before the restart: {status} {ran}"
        );
        person.peer.close();
    }

    // The server restarts on the state it kept, as a deployment restarts it.
    // Its API server is a new one, which knows no key yet: Ada approves her
    // backend's key again and mints the workspace's own key anew.
    let on_server = on_server.restart(false).await;
    let page = enrol(&ada, "Ada's browser again", DeviceCapabilities::member());
    let backend = Key {
        api: &on_server.api,
        token: Box::pin(approved_key(&on_server, &ada, &page, "Ada's backend")).await,
    };
    let agent = Key {
        api: &on_server.api,
        token: backend.mint(&ws, "Ada's agent").await,
    };
    let (status, ran) = agent.run(&ws, "echo after-restart").await;
    assert!(
        printed(status, &ran, "after-restart"),
        "ran after the restart: {status} {ran}"
    );
}
