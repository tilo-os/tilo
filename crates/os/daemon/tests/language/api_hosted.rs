#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! What an API server in front of people's workspaces answers a key: a webhook posted to its
//! receiver signed; a call reaching the server that keeps the workspace, though another
//! answers; the same key working at every API server and after the one it used goes away; a
//! typed refusal when nothing that can read the workspace is online; a computer's one
//! session; its agent's grants; and what the workspace keeps for its members.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::workspace_invocations::{self, Invocation, InvocationStatus};
use tilo_actions::remote::{
    ActionProvider, ResourceAddress, ResourceCopy, model_providers, person_vault,
};

use crate::language::host;

/// One delivery a receiver was posted.
#[derive(Clone, Debug)]
struct Delivered {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Delivered {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// A webhook receiver on this machine: what each delivery carried, answered `status`.
struct Receiver {
    url: String,
    got: Arc<Mutex<Vec<Delivered>>>,
    woken: Arc<tokio::sync::Notify>,
}

impl Receiver {
    /// A receiver answering every delivery `status`, on a port of this machine's own.
    async fn start(status: u16) -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let got: Arc<Mutex<Vec<Delivered>>> = Arc::default();
        let woken = Arc::new(tokio::sync::Notify::new());
        let kept = Arc::clone(&got);
        let wake = Arc::clone(&woken);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let kept = Arc::clone(&kept);
                let wake = Arc::clone(&wake);
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                    let mut bytes = Vec::new();
                    let mut chunk = [0_u8; 4096];
                    let (head, body) = loop {
                        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&bytes[..at]).to_string();
                            let length = head
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())?
                                })
                                .unwrap_or(0);
                            while bytes.len() < at + 4 + length {
                                let read = stream.read(&mut chunk).await.unwrap_or(0);
                                if read == 0 {
                                    break;
                                }
                                bytes.extend_from_slice(&chunk[..read]);
                            }
                            break (head, bytes[at + 4..].to_vec());
                        }
                        let read = stream.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..read]);
                    };
                    let mut lines = head.lines();
                    let request = lines.next().unwrap_or_default();
                    let mut parts = request.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_owned();
                    let target = parts.next().unwrap_or_default().to_owned();
                    let headers: BTreeMap<String, String> = lines
                        .filter_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
                        })
                        .collect();
                    kept.lock().unwrap().push(Delivered {
                        method,
                        target,
                        headers,
                        body,
                    });
                    wake.notify_waiters();
                    let answer = format!(
                        "HTTP/1.1 {status} Webhook\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(answer.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}/hook"),
            got,
            woken,
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn count(&self) -> usize {
        self.got.lock().unwrap().len()
    }

    /// The next delivery posted to it.
    async fn next(&self) -> Delivered {
        let waiting = async {
            loop {
                let first = self.got.lock().unwrap().first().cloned();
                if let Some(first) = first {
                    return first;
                }
                self.woken.notified().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), waiting)
            .await
            .expect("the webhook's receiver is posted a delivery within 10 s")
    }
}

/// What a key asks: make a workspace's event. An invitation is one invocation of the
/// workspace's own invitation action, which a webhook on invocations posts.
async fn cause_an_invocation(key: &host::Key, workspace: &str) {
    let (status, invited) = key
        .post(
            &format!("/v1/workspaces/{workspace}/resources/workspace/invitations/actions/invite"),
            &json!({"role": "editor"}),
        )
        .await;
    assert_eq!(status, 200, "{invited}");
}

/// The webhook delivery invocations the workspace keeps, newest first.
async fn deliveries(host: &host::Host, workspace: &str) -> Vec<Invocation> {
    let provider: Arc<dyn ActionProvider + Send + Sync> = host.dispatch();
    let reads = provider
        .resource_reads()
        .expect("the dispatch reads records");
    let address = ResourceAddress {
        workspace: workspace.to_owned(),
        app: workspace_invocations::PROVIDER.to_owned(),
        record: String::new(),
    };
    let caller = host.caller();
    let read = reads
        .read(&caller, &address)
        .await
        .expect("the person reads the workspace's invocations");
    let value = match read {
        ResourceCopy::Reading { value, .. } => value,
        other => panic!("the invocations are a reading, not {other:?}"),
    };
    serde_json::from_value::<Vec<Invocation>>(value["items"].clone())
        .expect("the items read as invocations")
        .into_iter()
        .filter(|invocation| invocation.action.as_deref() == Some("tilo.webhooks#post"))
        .collect()
}

/// A hosted API posts a workspace's event to a webhook, signed with the key its maker was
/// answered once; a URL that never answers is retried and leaves a failed invocation; and a key
/// that does not reach the workspace is refused.
#[test]
fn a_hosted_apis_webhook_reaches_its_receiver_signed() {
    host::run(|host| async move {
        use ed25519_dalek::Verifier as _;

        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let create = "/v1/resources/account/webhooks/actions/create";
        // The receiver is a server this test starts on this machine: a webhook may post to this
        // computer's own loopback, and to nothing else of its network.
        let receiver = Receiver::start(200).await;
        let (status, refused) = key
            .post(
                create,
                &json!({"workspace": workspace, "url": "https://10.0.0.7/hook"}),
            )
            .await;
        assert_eq!(
            status, 400,
            "a webhook posts nothing to this network: {refused}"
        );
        let (status, hook) = key
            .post(
                create,
                &json!({"workspace": workspace, "url": receiver.url(), "events": ["invocation"]}),
            )
            .await;
        assert_eq!(status, 200, "the webhook is kept: {hook}");
        let hook = &hook["result"];
        let signing = hook["key"]
            .as_str()
            .expect("the webhook says the key it signs with");

        cause_an_invocation(&key, &workspace).await;
        let delivery = receiver.next().await;
        assert_eq!(
            (delivery.method.as_str(), delivery.target.as_str()),
            ("POST", "/hook")
        );
        assert_eq!(delivery.header("tilo-webhook-id"), hook["id"].as_str());
        assert!(
            delivery
                .header("tilo-delivery")
                .is_some_and(|delivery| !delivery.is_empty()),
            "a delivery is named"
        );
        assert_eq!(delivery.header("tilo-key"), Some(signing));
        let said = delivery
            .header("tilo-signature")
            .expect("a delivery is signed");
        let time: u64 = said
            .strip_prefix("t=")
            .and_then(|rest| rest.split(',').next())
            .and_then(|time| time.parse().ok())
            .expect("a signature says its time");
        let mut signed = format!("{time}.").into_bytes();
        signed.extend_from_slice(&delivery.body);
        let verifier = ed25519_dalek::VerifyingKey::from_bytes(
            &<[u8; 32]>::try_from(hex::decode(signing).unwrap().as_slice()).unwrap(),
        )
        .unwrap();
        verifier
            .verify(
                &signed,
                &ed25519_dalek::Signature::from_slice(
                    &hex::decode(
                        said.split_once("v1=")
                            .expect("a signature names its version")
                            .1,
                    )
                    .unwrap(),
                )
                .unwrap(),
            )
            .expect("a delivery is signed with its webhook's key");
        let event: Value = serde_json::from_slice(&delivery.body).expect("a delivery's body");
        assert_eq!(event["type"], "invocation", "{event}");
        assert_eq!(event["workspace"], workspace.as_str(), "{event}");

        // The one invocation was posted once.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(receiver.count(), 1, "one event, one delivery");

        // A URL that never answers is tried again, then its invocation is failed.
        let failing = Receiver::start(500).await;
        let (status, dead) = key
            .post(
                create,
                &json!({"workspace": workspace, "url": failing.url(), "events": ["invocation"]}),
            )
            .await;
        assert_eq!(status, 200, "{dead}");
        cause_an_invocation(&key, &workspace).await;
        let failed = loop {
            let deliveries = deliveries(&host, &workspace).await;
            if let Some(failed) = deliveries
                .iter()
                .find(|invocation| invocation.status == InvocationStatus::Failed)
            {
                break failed.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(
            failing.count() >= 2,
            "a URL that does not answer is retried: {} deliveries",
            failing.count()
        );
        assert_eq!(
            failed.action.as_deref(),
            Some("tilo.webhooks#post"),
            "the failed delivery is an invocation of its own: {failed:?}"
        );

        // A key that reaches another workspace registers nothing here.
        let elsewhere = uuid::Uuid::new_v4().to_string();
        let narrow = host.key_reaching(&workspace, "Visa backend, one workspace");
        let (status, refused) = narrow
            .post(
                create,
                &json!({"workspace": elsewhere, "url": receiver.url()}),
            )
            .await;
        assert_eq!(
            status, 403,
            "a key reaches only its own workspace: {refused}"
        );
        assert_eq!(refused["error"]["code"], "not_permitted", "{refused}");
    });
}

/// A key's call reaches the server that keeps the person's workspace: a device of theirs asks
/// the workspace's own tree, and the person's workspace is listed where it is kept.
#[test]
fn a_call_reaches_the_host_that_keeps_the_workspace() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let laptop = host.member("Ada's laptop", &workspace).await;
        let listed = laptop
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "server.list"),
                &json!({}),
            )
            .await;
        assert!(
            matches!(listed, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "a call for the workspace reaches the one that keeps it: {listed:?}"
        );
        let (status, kept) = key
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(status, 200, "{kept}");
        assert!(
            kept["result"]["workspaces"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == workspace.as_str())),
            "her workspaces are listed where they are kept: {kept}"
        );
    });
}

/// The same key is served wherever its person's workspaces are: two keys of the person's,
/// made at different times, read the same workspace and list the same workspaces.
#[test]
fn a_key_works_at_every_api_server() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let first = host.key("Visa backend");
        let elsewhere = host.key("Visa backend, again");
        for key in [&first, &elsewhere] {
            let (status, listed) = key
                .post("/v1/resources/account/workspaces/actions/list", &json!({}))
                .await;
            assert_eq!(status, 200, "{listed}");
            let (status, read) = key
                .get(&format!("/v1/workspaces/{workspace}/resources"))
                .await;
            assert_eq!(status, 200, "{read}");
        }
    });
}

/// The server a call used goes away and comes back on the same home: the workspace it served
/// is served again, and a key's grant reaches it, because the grant is the workspace's record
/// and not the daemon's memory.
#[test]
fn a_call_is_served_after_the_host_it_used_goes_away() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let (status, listed) = key
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(status, 200, "{listed}");
        let (status, read) = key
            .get(&format!("/v1/workspaces/{workspace}/resources"))
            .await;
        assert_eq!(status, 200, "{read}");

        // The key keeps the daemon it asks alive, so it goes before the computer stops it.
        drop(key);
        host.restart().await;

        // The computer that started again serves the workspace it kept, for a key made here.
        let key = host.key("Visa backend, again");
        let (status, listed) = key
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(status, 200, "{listed}");
        let ids: Vec<&str> = listed["result"]["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|workspace| workspace["id"].as_str())
            .collect();
        assert!(
            ids.contains(&workspace.as_str()),
            "it lists the workspace: {listed}"
        );
        let (status, read) = key
            .get(&format!("/v1/workspaces/{workspace}/resources"))
            .await;
        assert_eq!(status, 200, "{read}");
    });
}

/// Nothing that can read the workspace is online: the call is refused in plain words, with
/// what to do about it.
///
/// The fixture's daemon serves the workspace from its own process, so there is no list of
/// servers to name one that does not answer: the flow is a server that holds the workspace and
/// then stops, which this device-native fixture cannot put a key in front of.
#[test]
#[ignore = "the daemon-host fixture serves the workspace from its own process; there is no list \
            of servers to name one that does not answer"]
fn no_host_that_can_read_the_workspace_is_a_typed_refusal() {
    host::run(|_host| async move {
        panic!(
            "the daemon-host fixture serves the workspace from its own process; there is no \
             list of servers to name one that does not answer"
        );
    });
}

/// A computer `tilo login` made a device of keeps that device's one session with its server,
/// and its commands are calls to the API the computer serves over that session.
///
/// The fixture's daemon has no login and no session with a server: a logged-in computer is the
/// two-device proof's shape, not this device-native fixture's.
#[test]
#[ignore = "the fixture's daemon has no login and has no session with a server; its person's \
            programs reach the API served over the daemon's own workspace"]
fn a_logged_in_computers_commands_go_over_its_one_session() {
    host::run(|_host| async move {
        panic!(
            "the fixture's daemon has no login and has no session with a server; its person's \
             programs reach the API served over the daemon's own workspace"
        );
    });
}

/// The computer's agent, over the session its computer keeps, reads a workspace's people and
/// invites someone; its person is asked first, and their answer stands.
#[test]
fn a_logged_in_computers_agent_reads_and_invites_people_as_its_grants_let_it() {
    host::run(|host| async move {
        let agent = host.agent();
        let (listed, failed) = said(
            &agent
                .tool("tilo_do", &json!({"path": "workspace/members#list"}))
                .await,
        );
        assert!(!failed, "the agent lists the workspace's people: {listed}");
        assert!(listed.contains(host.person()), "{listed}");

        // Inviting someone asks the person first: allowed, the link comes back.
        let invited = agent
            .tool_structured(
                "tilo_do",
                &json!({"path": "workspace/invitations#invite", "args": {"role": "editor"}}),
            )
            .await;
        assert_eq!(
            invited["structuredContent"]["status"], "pending",
            "its person is asked first: {invited}"
        );
    });
}

/// The text of an MCP tool's result, and whether it failed.
fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, result["isError"].as_bool().unwrap_or(false))
}

/// What the workspace keeps (what its members sign in with, their model keys) answers its
/// members on devices of theirs, and a key a person made for a script as their agent there.
#[test]
fn a_key_made_for_a_script_reaches_the_workspaces_stores_as_its_persons_agent() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let list = person_vault::target(person_vault::LIST);
        let kept = laptop
            .call(&list.provider, list.address.clone(), &json!({}))
            .await;
        assert!(
            matches!(kept, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "her own device lists the workspace's vault: {kept:?}"
        );

        let with_key = host.member("Nightly build", &workspace).await;
        let listed = with_key
            .call(&list.provider, list.address.clone(), &json!({}))
            .await;
        assert!(
            matches!(listed, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "her key, as her agent, lists the workspace's vault: {listed:?}"
        );
        for (what, target, argument) in [
            (
                "adds to the vault",
                person_vault::target(person_vault::ADD),
                json!({"kind": "login",
                       "belongsTo": ["https://mail.example.test"],
                       "username": "ada@example.test",
                       "secret": "correct horse battery staple"}),
            ),
            (
                "sets a model key",
                model_providers::target(model_providers::SET_KEY),
                json!({"provider": "a-provider", "key": "sk-test-not-a-key"}),
            ),
        ] {
            let answered = with_key
                .call(&target.provider, target.address, &argument)
                .await;
            assert!(
                !matches!(
                    answered,
                    Err(tilo_actions::remote::ActionCallError::UnknownAction)
                ),
                "her key, as her agent, {what}: {answered:?}"
            );
        }
    });
}
