//! A person's assistant on their computer calls a service with a key it never sees.
//!
//! The person keeps the key in their vault; the assistant's turn holds only the reference and
//! makes the call; the host asks the person and sends nothing; they allow it; the assistant
//! makes the same call again and is answered what the service answered, the key struck out of
//! what the service repeated. The key is in no message, no tool result and nowhere on the
//! computer's disk in the clear.
//!
//! The daemon serves its workspace's vault and its `web-call`, and asks its person first: the
//! question is put in the conversation, which Chat holds. A test that reaches the conversation
//! before the app runs stops at the app's open.

#![allow(clippy::too_many_lines, reason = "one call's whole chain in order")]

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_actions::remote::ActionProvider as _;
use tilo_actions::remote::ReceiptStatus;
use tilo_workspace_service::workspace_vault;

use crate::language::chat::{opens, said};
use crate::language::host;

/// A made-up key: no service ever issued it, and it opens nothing.
const KEY: &str = "made-up-key-for-tests-0123456789abcdef";
/// What stands in an answer where the key stood.
const WITHHELD: &str = "[withheld]";
/// What the agent is told when its person is asked first.
const ASKED: &str = "Your person is asked in the conversation";

/// A service that keeps a number behind the made-up key. It is a careless one: it repeats what
/// it was shown in what it answers.
struct Service {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Service {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let shown = text
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("authorization:")
                            .map(|value| value.trim().to_owned())
                    })
                    .unwrap_or_default();
                kept.lock().unwrap().push(shown.clone());
                let (status, body) = if shown == format!("Bearer {KEY}") {
                    ("200 OK", format!(r#"{{"balance":1234,"shown":"{shown}"}}"#))
                } else {
                    (
                        "401 Unauthorized",
                        r#"{"error":"that key is unknown"}"#.to_owned(),
                    )
                };
                let answer = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: \
                     {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        Self { url, seen }
    }

    /// The sign-ins the service was shown, each request's.
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

/// The call for the balance at `service` with the vault item `reference` names.
fn balance(service: &Service, reference: &str) -> Value {
    json!({
        "url": format!("{}v1/balance", service.url),
        "key": reference,
        "reason": "To read this month's numbers.",
    })
}

/// Every file under the daemon's profile whose bytes hold `needle`.
fn files_holding(dir: &std::path::Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => waiting.push(path),
                Ok(metadata) if metadata.is_file() => {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    if bytes.windows(needle.len()).any(|window| window == needle) {
                        holding.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    holding
}

/// The person's own call of a vault action, with `args`, as their desktop makes it.
async fn vault(host: &host::Host, action: &str, args: &Value) -> Result<Value, String> {
    let receipt = host
        .dispatch()
        .invoke(
            &host.caller(),
            host::call(
                workspace_vault::PROVIDER,
                tilo_actions::ActionAddress::new(workspace_vault::RESOURCE, action),
                args,
            ),
        )
        .await
        .expect("the dispatch answers")
        .map_err(|refused| format!("{refused:?}"))?;
    let ReceiptStatus::Completed { output } = &receipt.status else {
        return Err(format!("{:?}", receipt.status));
    };
    Ok(output.value.clone().unwrap_or(Value::Null))
}

/// The person's assistant calls a service with a key it never sees: it is told its person is
/// asked, nothing is sent, and only once they allow it is the call made, answered with the key
/// struck out. The key is nowhere on the computer's disk in the clear.
#[test]
fn the_assistant_calls_a_service_with_a_key_it_never_sees() {
    host::run(|host| async move {
        let service = Service::start();
        // The protected path: the person's own device hands the key to their vault here.
        let kept = vault(
            host.as_ref(),
            workspace_vault::ADD,
            &json!({"kind": "key", "belongsTo": [service.url.clone()], "secret": KEY}),
        )
        .await
        .unwrap_or_else(|refused| panic!("the vault keeps the key: {refused}"));
        let reference = kept["reference"].as_str().expect("a reference").to_owned();

        // The turn holds the reference and makes the call. Nobody watches, so the computer asks
        // the person and sends nothing.
        let agent = host.agent();
        let (told, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "vault/vault#web-call", "args": balance(&service, &reference)}),
                )
                .await,
        );
        assert!(
            !failed && told.contains(ASKED),
            "the agent is told its person is asked: {told}"
        );
        assert!(service.seen().is_empty(), "nothing is sent unasked");

        // The question is in the conversation, which the workspace's Chat holds.
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        assert!(
            crate::language::chat::chat_holds(host.as_ref(), "To read this month's numbers.")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the question says why, in the agent's words"
        );
        // The reference is what the conversation holds; the key never is.
        assert!(
            !crate::language::chat::chat_holds(host.as_ref(), KEY)
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "no message holds the key"
        );

        // The person allows it: the assistant's next call is sent once, and the answer is its
        // tool result, with the key struck out of what the service repeated. The person's own
        // call of the allowed item stands for the assistant's retry until the conversation's
        // answer action runs (the app's open above).
        let allowed = vault(
            host.as_ref(),
            workspace_vault::WEB_CALL,
            &balance(&service, &reference),
        )
        .await
        .unwrap_or_else(|refused| panic!("the call the person allowed: {refused}"));
        let answer = allowed.to_string();
        assert!(
            answer.contains("1234") && answer.contains(&format!("Bearer {WITHHELD}")),
            "the service's answer, with the key struck: {answer}"
        );
        assert!(!answer.contains(KEY), "an answer holds the key");
        assert_eq!(service.seen(), [format!("Bearer {KEY}")], "sent once");

        // What the item remembers of it: when, where and for whom.
        let remembers = vault(
            host.as_ref(),
            workspace_vault::USES,
            &json!({"item": reference}),
        )
        .await
        .unwrap_or_else(|refused| panic!("{refused}"));
        assert_eq!(
            remembers["uses"].as_array().unwrap().len(),
            1,
            "{remembers}"
        );
        assert_eq!(remembers["uses"][0]["what"], "GET /v1/balance");
        assert!(
            !remembers.to_string().contains(KEY),
            "what the item remembers holds no key"
        );

        // The key is in no file of the computer's in the clear: the vault seals it.
        let profile = std::path::PathBuf::from(
            std::env::var_os("TILO_HOST_TEST_PROFILE").expect("the fixture's profile"),
        );
        let holding = files_holding(&profile, KEY.as_bytes());
        assert!(holding.is_empty(), "in the clear: {holding:?}");
    });
}
