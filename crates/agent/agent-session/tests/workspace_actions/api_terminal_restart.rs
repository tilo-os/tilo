//! A workspace a server made for a person, and handed to them, runs their
//! key's commands in its Terminal after the server restarts as it did before:
//! the server serves it as one of its administrators, and a restart must not
//! leave it unable to read or write the workspace's Terminal session.

use super::api::ISSUER;
use super::api_on_server::{api_on_server_shelled, approved_key};
use super::api_unattended::{Key, Person, Sheet, asked, backend, beside, waiting, workspace};
use super::*;
use serde_json::Value;
use std::sync::Arc;
use tilo_server_client::MemberApprover;
use tilo_server_client::questions::{MemberQuestions, answer_questions};
use tilo_server_protocol::certify::ConfirmationAnswer;
use tokio::sync::mpsc;

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
        let host = MemberApprover::new(on_server.server_host());
        let (answers, mut answered) = mpsc::unbounded_channel();
        let person = Person {
            asked: Arc::default(),
            answers,
        };
        let mut sheet = Sheet {
            asked: Arc::clone(&person.asked),
        };
        let mut questions = MemberQuestions {
            server: &host,
            root: &ada,
            device: None,
        };
        let page = answer_questions(&mut questions, &mut sheet, &mut answered, || {
            tokio::time::sleep(Duration::from_millis(50))
        });
        tokio::pin!(page);
        let (status, asking) = beside(&mut page, agent.unattended(&ws, true)).await;
        let run = waiting(status, &asking).unwrap_or_else(|| panic!("pending: {status} {asking}"));
        asked(&mut page, &person, 1).await;
        person.answer(ConfirmationAnswer::AllowOnce);
        let (status, allowed) = beside(&mut page, agent.follow(&ws, &run)).await;
        assert_eq!(status, 200, "{allowed}");
        let (status, ran) = beside(&mut page, agent.run(&ws, "echo before-restart")).await;
        assert!(
            printed(status, &ran, "before-restart"),
            "ran before the restart: {status} {ran}"
        );
        host.close().await;
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
