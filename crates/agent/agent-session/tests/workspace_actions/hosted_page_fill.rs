//! A person's assistant on the server has a field of a web page filled from their vault, with
//! no device of theirs on while it works, and nothing but the page holds the text.
//!
//! Through the server's own interfaces, with the server, the person and the assistant of the
//! test this is a part of. The workspace's Browser window is a stand-in the test holds: the
//! device the server placed and certified for it, showing the sign-in page of a made-up site.
//! It is handed what the server hands a window, asks what a window asks, and opens what is
//! sealed to it. No engine starts. What a turn on the server cannot do yet shows here too: it
//! opens and reads no page, so the person's own device opened Browser and the page is open when
//! the turn begins.
use std::time::{Duration, Instant};

use super::*;
use tilo_actions::remote::environment_device::{self, ItemUsed};
use tilo_actions::remote::{
    ActionCallError, InvocationReceipt, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId};
use tilo_server::Envelope;
use tilo_server::hosted_browser::{
    BrowserSessions, FILL_PAGE_ITEM, HostedBrowsers, Unmetered, Window,
};
use tilo_server_protocol::hosted::Material;
use ui::tilo_workspace_agent::{Bound, WorkspaceAgents, workspace_agents};

/// The made-up site whose sign-in page the window shows.
const SITE: &str = "https://sign-in.example.test";
/// Another made-up site, which the item of one test belongs to instead.
const ELSEWHERE: &str = "https://elsewhere.example.test";
/// A made-up password, nobody's: nothing but the page may hold it.
const SECRET: &str = "made-up-password-of-nobody-9f3";
/// A second made-up password, nobody's, kept in an item no fill names.
const OTHER_SECRET: &str = "made-up-second-password-of-nobody-4c1";
/// The provider the stand-in window publishes under.
const WINDOW: &str = "4711";
/// The password field of the sign-in page, as the page's outline names it.
const FIELD: &str = "d1e5";

/// Where the server starts a workspace's Browser: nowhere, here. It keeps what the server
/// handed the session, for the stand-in to join as.
#[derive(Clone, Default)]
struct Machines(Arc<Mutex<Vec<Material>>>);

impl BrowserSessions for Machines {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.0.lock().unwrap().push(material.clone());
        Ok(Window {
            device: material.device.device_public_key,
            provider: WINDOW.into(),
        })
    }

    fn stop(&self, _: &str) {}
}

/// What the stand-in window was handed, what its page holds and what reached it.
#[derive(Clone, Default)]
struct Shown {
    /// The fills the window was handed, by the field each names.
    handed: Arc<Mutex<Vec<String>>>,
    /// What was written into the page: the field, the kind of field, the text.
    filled: Arc<Mutex<Vec<(String, String, String)>>>,
    /// Everything the server answered the window's asks with, as it travelled.
    travelled: Arc<Mutex<Vec<String>>>,
    /// What reached the window, each by how it begins: what a test that fails says.
    seen: Arc<Mutex<Vec<String>>>,
    /// The fills the window was handed: each by its id, with the device that handed it.
    fills: Arc<Mutex<Vec<(InvocationId, [u8; 32])>>>,
    /// What the server answered an ask beyond the one a window makes for a fill.
    beyond: Arc<Mutex<Vec<String>>>,
    /// How a test has the stand-in stray from what a window does.
    strays: Arc<Mutex<Strays>>,
    /// What lets a stand-in that holds its fill go on to ask.
    let_on: Arc<tokio::sync::Notify>,
}

/// How a test has the stand-in stray from what a window does.
#[derive(Clone, Default)]
struct Strays {
    /// The item it asks for in place of the one its fill names.
    another_item: Option<String>,
    /// Whether it asks a second time for a fill it was answered.
    twice: bool,
    /// Whether it holds a fill it was handed until it is let on.
    holds: bool,
}

/// A window's ask for the text of `item` at `origin`, for the fill `fill`: the ask's own id, and
/// the ask.
fn asks(fill: InvocationId, item: &str, origin: &str) -> (InvocationId, RemoteActionMessage) {
    let ask = InvocationId::new();
    let mut wanted = serde_json::Map::new();
    wanted.insert("invocation".into(), serde_json::to_value(fill).unwrap());
    wanted.insert("item".into(), item.into());
    wanted.insert("origin".into(), origin.into());
    (
        ask,
        RemoteActionMessage::Invoke(InvokeRequest {
            invocation: ask,
            target: environment_device::target(environment_device::USE_ITEM),
            argument: InvokeArgument::Json(wanted),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }),
    )
}

impl Shown {
    /// What the window does with the server's answer to its ask for the fill `fill` of
    /// `field`, at the page whose site is `origin`: a text released to it it opens with its own
    /// key and writes into the page; asked first or refused, it writes nothing.
    fn take(
        &self,
        (key, workspace, origin): (&SigningKey, &str, &str),
        (fill, field): (InvocationId, &str),
        answered: ReceiptStatus,
    ) -> ReceiptStatus {
        let failed = |code: &str, message: String| ReceiptStatus::Failed {
            error: ActionFailure {
                code: code.to_owned(),
                message,
                details: None,
                retryable: false,
            },
        };
        match answered {
            ReceiptStatus::Completed { output } => {
                match serde_json::from_value::<ItemUsed>(output.value.unwrap_or_default()) {
                    Ok(ItemUsed::Released { sealed, into }) => {
                        let bound = key_group::ReleasedFor {
                            workspace,
                            invocation: *fill.0.as_bytes(),
                            origin,
                        };
                        let text = key_group::open_released_text(
                            key,
                            &bound,
                            &hex::decode(sealed).unwrap(),
                        )
                        .expect("the window opens what was sealed to it for this fill");
                        self.filled.lock().unwrap().push((
                            field.to_owned(),
                            into,
                            text.as_str().to_owned(),
                        ));
                        ReceiptStatus::Completed {
                            output: ActionOutput {
                                value: Some(serde_json::json!({"filled": field})),
                                resources: Vec::new(),
                            },
                        }
                    }
                    Ok(ItemUsed::Asked { words }) => failed("asked_person_first", words),
                    Err(error) => failed("browser.page.item_refused", error.to_string()),
                }
            }
            ReceiptStatus::Rejected {
                error: ActionCallError::Refused { reason },
            } => failed("browser.page.item_refused", reason),
            other => failed("browser.page.item_refused", format!("{other:?}")),
        }
    }
}

/// The session that holds the workspace's Browser window, stood in for: the device the server
/// placed joins the workspace and shows the page at `origin`. Handed a fill, it asks the server
/// for the item's text for that origin, as a window does, and answers the fill with what came
/// of it.
async fn window_shows(hosted: &Hosted, material: &Material, origin: &'static str) -> Shown {
    let shown = Shown::default();
    let workspace_id = material.workspace_id;
    let person = hosted.root.verifying_key().to_bytes();
    let client = tilo_server::ServerClient::connect(
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap(),
        hosted.address.clone(),
        person,
        material.device.clone(),
        material.key().unwrap(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .expect("the window's device dials the server");
    // The session in a workspace's machine asks its server to enter its device before it
    // joins, and so does its stand-in: the workspace's governance then names the device as one
    // of its person's.
    tilo_server::workspaces::enter_invitation(&client, &material.workspace, 600)
        .await
        .expect("the server enters the device it placed into its workspace");
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    // A governed workspace hands a device what it holds before it is ready.
    loop {
        match tilo_test_env::within("the window joins", client.recv()).await {
            Some(Envelope::Ready { .. }) => break,
            Some(_) => {}
            None => panic!("the window's device was let go before it joined"),
        }
    }
    let window = material.device.device_public_key;
    let (key, workspace) = (material.key().unwrap(), material.workspace.clone());
    let held = shown.clone();
    tokio::spawn(async move {
        // The fills that wait for the server's answer: the ask each made, the fill, its field
        // and the item asked for.
        let mut asked: Vec<(InvocationId, InvocationId, String, String)> = Vec::new();
        // The fills asked for a second time: that ask, the fill, its field and the first answer.
        let mut again: Vec<(InvocationId, InvocationId, String, ReceiptStatus)> = Vec::new();
        while let Some(envelope) = client.recv().await {
            held.seen
                .lock()
                .unwrap()
                .push(format!("{envelope:?}").chars().take(40).collect());
            let Envelope::RemoteAction {
                from_device_public_key,
                frame,
                ..
            } = envelope
            else {
                continue;
            };
            let Ok(RemoteActionFrame { message, .. }) = RemoteActionFrame::decode(&frame) else {
                continue;
            };
            let answer = match message {
                RemoteActionMessage::Invoke(fill)
                    if fill.target.address.action == FILL_PAGE_ITEM =>
                {
                    let (element, named) = {
                        let named = |name: &str| match &fill.argument {
                            InvokeArgument::Json(arguments) => arguments
                                .get(name)
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            _ => String::new(),
                        };
                        (named("element"), named("item"))
                    };
                    held.handed.lock().unwrap().push(element.clone());
                    held.fills
                        .lock()
                        .unwrap()
                        .push((fill.invocation, from_device_public_key));
                    let strays = held.strays.lock().unwrap().clone();
                    if strays.holds {
                        held.let_on.notified().await;
                    }
                    let item = strays.another_item.unwrap_or(named);
                    let (ask, asking) = asks(fill.invocation, &item, origin);
                    asked.push((ask, fill.invocation, element, item));
                    asking
                }
                RemoteActionMessage::Receipt(receipt) => {
                    let second = again
                        .iter()
                        .position(|(ask, ..)| *ask == receipt.invocation);
                    let first = asked
                        .iter()
                        .position(|(ask, ..)| *ask == receipt.invocation);
                    let (fill, field, answered) = match (second, first) {
                        // The answer to a second ask is kept, and the fill then ends as its
                        // first answer had it.
                        (Some(at), _) => {
                            let (_, fill, field, answered) = again.remove(at);
                            held.beyond.lock().unwrap().push(format!("{receipt:?}"));
                            (fill, field, answered)
                        }
                        (None, Some(at)) => {
                            let (_, fill, field, item) = asked.remove(at);
                            held.travelled.lock().unwrap().push(format!("{receipt:?}"));
                            let twice = held.strays.lock().unwrap().twice;
                            if twice {
                                let (ask, asking) = asks(fill, &item, origin);
                                again.push((ask, fill, field, receipt.status));
                                let _ = client.send(Envelope::RemoteAction {
                                    workspace_id,
                                    version: tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION,
                                    to_device_public_key: from_device_public_key,
                                    from_device_public_key: window,
                                    from_user_public_key: person,
                                    frame: RemoteActionFrame::new(asking).encode().unwrap(),
                                });
                                continue;
                            }
                            (fill, field, receipt.status)
                        }
                        (None, None) => continue,
                    };
                    let status = held.take((&key, &workspace, origin), (fill, &field), answered);
                    RemoteActionMessage::Receipt(InvocationReceipt {
                        invocation: fill,
                        status,
                    })
                }
                _ => continue,
            };
            let _ = client.send(Envelope::RemoteAction {
                workspace_id,
                version: tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION,
                to_device_public_key: from_device_public_key,
                from_device_public_key: window,
                from_user_public_key: person,
                frame: RemoteActionFrame::new(answer).encode().unwrap(),
            });
        }
    });
    shown
}

/// Another device of the same person joins the workspace and, while the window holds the first
/// fill it is handed, asks the server for the text of `item` for that fill, in the window's
/// place. What it is answered is kept, and the window is then let on.
async fn another_device_asks(
    hosted: &Hosted,
    shown: &Shown,
    item: &str,
) -> Arc<Mutex<Vec<String>>> {
    let answered = Arc::new(Mutex::new(Vec::new()));
    let workspace_id = WorkspaceId::parse(&hosted.workspace).unwrap();
    let person = hosted.root.verifying_key().to_bytes();
    let other = enrol(&hosted.root, "another", DeviceCapabilities::member());
    let client = tilo_server::ServerClient::connect(
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap(),
        hosted.address.clone(),
        person,
        other.cert.clone(),
        other.key.clone(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .expect("the person's other device dials the server");
    tilo_server::workspaces::enter_invitation(&client, &hosted.workspace, 600)
        .await
        .expect("the server enters the person's other device into their workspace");
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    loop {
        match tilo_test_env::within("the other device joins", client.recv()).await {
            Some(Envelope::Ready { .. }) => break,
            Some(_) => {}
            None => panic!("the person's other device was let go before it joined"),
        }
    }
    let (shown, item, kept) = (shown.clone(), item.to_owned(), Arc::clone(&answered));
    let device = other.cert.device_public_key;
    tokio::spawn(async move {
        // The fill the window holds, and the device that handed it.
        let (fill, server) = loop {
            let handed = shown.fills.lock().unwrap().first().copied();
            if let Some(handed) = handed {
                break handed;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let (ask, asking) = asks(fill, &item, SITE);
        let _ = client.send(Envelope::RemoteAction {
            workspace_id,
            version: tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: server,
            from_device_public_key: device,
            from_user_public_key: person,
            frame: RemoteActionFrame::new(asking).encode().unwrap(),
        });
        let answer = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(envelope) = client.recv().await {
                let Envelope::RemoteAction { frame, .. } = envelope else {
                    continue;
                };
                let Ok(RemoteActionFrame {
                    message: RemoteActionMessage::Receipt(receipt),
                    ..
                }) = RemoteActionFrame::decode(&frame)
                else {
                    continue;
                };
                if receipt.invocation == ask {
                    return format!("{:?}", receipt.status);
                }
            }
            "the other device's session ended".to_owned()
        })
        .await
        .unwrap_or_else(|_| "the server did not answer the other device".to_owned());
        kept.lock().unwrap().push(answer);
        shown.let_on.notify_one();
    });
    answered
}

/// The server, its person and their assistant, with the workspace's Browser window showing the
/// made-up site's sign-in page and a sign-in kept in the person's vault.
struct SignIn {
    hosted: Hosted,
    shown: Shown,
    /// The vault item, by its reference.
    reference: String,
    /// Every log line of the test's process.
    lines: Lines,
}

/// A server that places Browser windows, with the window of its person's workspace showing the
/// Wait until nothing waits for the person any more.
async fn answered(hosted: &mut Hosted) {
    let deadline = tilo_test_env::hang_deadline();
    while !hosted.page.asks().await.is_empty() {
        assert!(Instant::now() < deadline, "the question was never answered");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// sign-in page at [`SITE`], and a sign-in in the person's vault that belongs to `belongs`.
async fn at_the_sign_in_page(belongs: &str) -> SignIn {
    let lines = logged();
    let machines = Machines::default();
    let browsers = Arc::new(HostedBrowsers::new(
        Box::new(machines.clone()),
        Box::new(Unmetered),
        "acct_ada".into(),
    ));
    let mut hosted = Hosted::start_placing(Some(Arc::clone(&browsers))).await;
    // The person's own device opens Browser in the workspace: a turn on the server opens no
    // page yet.
    let workspace_id = WorkspaceId::parse(&hosted.workspace).unwrap();
    let opened = hosted
        .page
        .invoke_on(
            &format!("workspace:{}", hosted.workspace),
            ActionAddress::new("app.so.tilo.browser", "open"),
            InvokeArgument::None,
        )
        .await;
    let deadline = tilo_test_env::hang_deadline();
    while browsers.window(workspace_id).is_none() {
        assert!(
            Instant::now() < deadline,
            "the workspace's Browser window was never placed: {opened:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let material = machines.0.lock().unwrap()[0].clone();
    let shown = window_shows(&hosted, &material, SITE).await;
    // The person keeps the sign-in in their vault, from their own device.
    let kept = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD,
            ),
            serde_json::json!({
                "kind": "login", "belongsTo": [belongs], "username": "ada", "secret": SECRET,
            }),
        )
        .await;
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    SignIn {
        hosted,
        shown,
        reference,
        lines,
    }
}

impl SignIn {
    /// One turn in which the assistant asks for the page's password field to be filled from the
    /// vault item: what its tool told it, and below that what the server warned of and what
    /// reached the window, for a test that fails to say why.
    async fn asks_to_fill(&self, words: &str, call: &str) -> String {
        let fill = does(
            call,
            "vault/vault#fill-page",
            &serde_json::json!({
                "element": FIELD,
                "item": self.reference,
                "reason": "To sign in to the made-up site for you.",
            }),
        );
        self.hosted
            .turn(words, vec![fill, Reply::text("I did what I could.")])
            .await;
        format!(
            "{}\n-- the server warned:\n{}\n-- the window saw:\n{}\n-- before the window was \
             let go:\n{}",
            self.hosted.last_tool_result(),
            self.warned(),
            self.shown.seen.lock().unwrap().join(" | "),
            self.before_the_window_was_let_go()
        )
    }

    /// What the server warned of or failed at, as its log says it, the newest last.
    fn warned(&self) -> String {
        let logged = String::from_utf8_lossy(&self.lines.0.lock().unwrap()).into_owned();
        let said: Vec<&str> = logged
            .lines()
            .filter(|line| line.contains(" WARN ") || line.contains("ERROR"))
            .collect();
        said[said.len().saturating_sub(12)..].join("\n")
    }

    /// What the server's own crates logged up to the line that says it let the window's
    /// device go, each line cut short; nothing where it never did.
    fn before_the_window_was_let_go(&self) -> String {
        let logged = String::from_utf8_lossy(&self.lines.0.lock().unwrap()).into_owned();
        let lines: Vec<&str> = logged
            .lines()
            .filter(|line| line.contains(" tilo_"))
            .collect();
        let Some(at) = lines
            .iter()
            .position(|line| line.contains("subscriber authorization changed"))
        else {
            return String::new();
        };
        lines[at.saturating_sub(40)..=at]
            .iter()
            .map(|line| line.chars().take(240).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What the page holds: each field written, the kind of field and the text.
    fn filled(&self) -> Vec<(String, String, String)> {
        self.shown.filled.lock().unwrap().clone()
    }

    /// Everything the server answered the window, as it travelled.
    fn travelled(&self) -> String {
        self.shown.travelled.lock().unwrap().join("\n")
    }

    /// The password reached the page and nothing else: no request the model was sent, which
    /// holds every tool result and every message it read; no message of the conversation;
    /// nothing the server answered the window, which travelled sealed; no log line; and no
    /// file of the server in the clear.
    async fn the_password_is_nowhere_but_in_the_page(&self) {
        let hosted = &self.hosted;
        let asked = hosted.model.asked();
        assert!(
            asked.len() >= 6,
            "three turns of two calls each: {}",
            asked.len()
        );
        for call in &asked {
            let sent = format!("{:?}", call.request);
            assert!(!sent.contains(SECRET), "a model request holds the password");
        }
        let last = format!("{:?}", asked.last().unwrap().request);
        assert!(
            last.contains("vault.item:"),
            "the model works with the reference"
        );
        let part = conversation_part(&hosted.workspaces, &hosted.workspace).await;
        let said = format!("{:?}", part.messages.iter().collect::<Vec<_>>());
        assert!(!said.contains(SECRET), "a message holds the password");
        let saved = hosted
            .workspaces
            .resource_document(&hosted.workspace, CHAT, chat::parts::part(0).as_str())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !saved
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "a stored message holds the password"
        );
        let travelled = self.travelled();
        assert!(
            travelled.contains("released") && !travelled.contains(SECRET),
            "what the server answers the window travels sealed: {travelled}"
        );
        let logged = String::from_utf8_lossy(&self.lines.0.lock().unwrap()).into_owned();
        assert!(
            logged.len() > 1_000 && !logged.contains(SECRET),
            "a log line holds the password, or no log was read: {} bytes",
            logged.len()
        );
        let holding = files_holding(hosted.state.path(), SECRET.as_bytes());
        assert!(
            holding.is_empty(),
            "in the clear on the server's disk: {holding:?}"
        );
    }
}

/// The person narrows their assistant to `bound`, as their page does: their device writes the
/// rules into the workspace's list of its agents. Answers once the server holds them.
async fn narrow(hosted: &Hosted, bound: Vec<Bound>) {
    let agents = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let held = hosted
        .workspaces
        .resource_document(&hosted.workspace, ui::tilo_workspace_agent::OWNER, &agents)
        .await
        .unwrap()
        .unwrap();
    let mut narrowed = ui::tilo_resource::Replica::<WorkspaceAgents>::load(&held)
        .unwrap()
        .as_author(&hosted.person());
    let rules = bound.len();
    narrowed
        .apply(
            WorkspaceAgents::SET_AGENT_BOUND.with(workspace_agents::SetAgentBound {
                agent: Id::from_raw(SCOUT.to_owned()),
                bound,
            }),
        )
        .unwrap();
    let route = DocumentRoute {
        app_id: ui::tilo_workspace_agent::OWNER.into(),
        cluster_id: hosted.workspace.clone(),
        document: agents.clone(),
    };
    let bytes = std::mem::take(narrowed.doc_mut()).save();
    hosted
        .page
        .peer
        .write(route, &collab_types::DocumentMutation::Snapshot(bytes))
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let bytes = hosted
            .workspaces
            .resource_document(&hosted.workspace, ui::tilo_workspace_agent::OWNER, &agents)
            .await
            .unwrap()
            .unwrap();
        let listed = WorkspaceAgents::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
        if listed
            .agents
            .iter()
            .any(|agent| agent.id.0 == SCOUT && agent.bound.len() == rules)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the server never holds the assistant's rules"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The chain. The first fill of the item at the site asks its person and fills nothing; their
/// allow fills nothing by itself and lets the next fill there through, once; the page's field
/// then holds the password; the fill after asks again; and the password is nowhere but in the
/// page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_assistant_has_a_sign_in_field_filled_with_a_password_only_the_page_holds() {
    let mut signing = Box::pin(at_the_sign_in_page(SITE)).await;

    let told = signing
        .asks_to_fill("Please sign in to the made-up site for me.", "call-1")
        .await;
    assert!(
        told.contains(ASKED),
        "the agent is told its person is asked: {told}"
    );
    assert!(signing.filled().is_empty(), "nothing is filled unasked");
    let asked = signing.hosted.page.asks().await;
    assert_eq!(asked.len(), 1, "one question about the fill: {told}");
    let question = &asked[0];
    assert!(
        matches!(
            &question.facts.by,
            tilo_actions::remote::asked::AskedBy::Agent(who) if who.id == SCOUT
        ),
        "only its own person is asked: {question:?}"
    );
    let shown = format!("{question:?}");
    assert!(
        shown.contains("sign-in.example.test") && shown.contains(SITE),
        "the question names the site by its name and by its whole origin: {shown}"
    );
    assert!(
        !shown.contains(SECRET) && !shown.contains(&signing.reference),
        "and shows nothing of the item but its kind: {shown}"
    );

    signing
        .hosted
        .page
        .resolve(question, "approve", None)
        .await
        .expect("the person allows it");
    answered(&mut signing.hosted).await;
    assert!(
        signing.filled().is_empty(),
        "an allow fills nothing by itself"
    );
    let told = signing.asks_to_fill("I allowed it.", "call-2").await;
    assert_eq!(
        signing.filled(),
        [(FIELD.to_owned(), "password".to_owned(), SECRET.to_owned())],
        "the page's password field holds the password: {told}"
    );
    assert!(
        !told.contains(ASKED) && !told.contains(SECRET),
        "the agent is told the field is filled, and not with what: {told}"
    );

    let told = signing
        .asks_to_fill("Please fill it in once more.", "call-3")
        .await;
    assert!(told.contains(ASKED), "one allow is one fill: {told}");
    assert_eq!(signing.filled().len(), 1, "one allow, one fill");

    signing.the_password_is_nowhere_but_in_the_page().await;
    signing.hosted.page.peer.close();
}

/// A no fills nothing, and the next fill asks again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_no_fills_nothing_and_the_next_fill_asks_again() {
    let mut signing = Box::pin(at_the_sign_in_page(SITE)).await;
    let told = signing
        .asks_to_fill("Please sign in to the made-up site for me.", "call-1")
        .await;
    let asked = signing.hosted.page.asks().await;
    assert_eq!(asked.len(), 1, "one question about the fill: {told}");
    signing
        .hosted
        .page
        .resolve(&asked[0], "decline", None)
        .await
        .expect("the person declines");
    answered(&mut signing.hosted).await;

    let told = signing
        .asks_to_fill("Please try once more.", "call-2")
        .await;
    assert!(
        signing.filled().is_empty() && told.contains(ASKED),
        "a no fills nothing, and the next fill asks again: the page holds {:?}: {told}",
        signing.filled()
    );
    assert!(
        !signing.travelled().contains("released"),
        "nothing was released to the window: {}",
        signing.travelled()
    );
    signing.hosted.page.peer.close();
}

/// One fill is decided once. A window that asks a second time for a fill it was answered is
/// refused, so one allow of the person is one text and no more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_ask_for_one_fill_is_refused_and_one_allow_is_one_text() {
    let mut signing = Box::pin(at_the_sign_in_page(SITE)).await;
    let told = signing
        .asks_to_fill("Please sign in to the made-up site for me.", "call-1")
        .await;
    let asked = signing.hosted.page.asks().await;
    assert_eq!(asked.len(), 1, "one question about the fill: {told}");
    signing
        .hosted
        .page
        .resolve(&asked[0], "approve", None)
        .await
        .expect("the person allows it");
    answered(&mut signing.hosted).await;

    signing.shown.strays.lock().unwrap().twice = true;
    let told = signing.asks_to_fill("I allowed it.", "call-2").await;
    assert_eq!(
        signing.filled(),
        [(FIELD.to_owned(), "password".to_owned(), SECRET.to_owned())],
        "the window's first ask is answered with the text: {told}"
    );
    let beyond = signing.shown.beyond.lock().unwrap().clone();
    assert_eq!(beyond.len(), 1, "the window asked a second time: {told}");
    assert!(
        beyond[0].contains("Rejected") && !beyond[0].contains("released"),
        "the second ask for the same fill is refused: {beyond:?}"
    );
    assert_eq!(
        signing.travelled().matches("released").count(),
        1,
        "one allow is one text: {}",
        signing.travelled()
    );
    signing.hosted.page.peer.close();
}

/// Only the window the server handed a fill is answered for it. Another device of the same
/// person that asks for that fill's item while it waits is refused, and the window's own ask is
/// answered as ever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_device_that_asks_for_a_waiting_fills_item_is_refused() {
    let mut signing = Box::pin(at_the_sign_in_page(SITE)).await;
    signing.shown.strays.lock().unwrap().holds = true;
    let answered = Box::pin(another_device_asks(
        &signing.hosted,
        &signing.shown,
        &signing.reference,
    ))
    .await;
    let told = signing
        .asks_to_fill("Please sign in to the made-up site for me.", "call-1")
        .await;
    let answered = answered.lock().unwrap().clone();
    assert_eq!(answered.len(), 1, "the other device asked once: {told}");
    assert!(
        answered[0].contains("Rejected") && !answered[0].contains("released"),
        "the other device is refused: {answered:?}"
    );
    assert!(
        told.contains(ASKED),
        "the window's own ask is answered as ever: {told}"
    );
    assert_eq!(
        signing.hosted.page.asks().await.len(),
        1,
        "the person is asked once, for the window's ask"
    );
    assert!(
        signing.filled().is_empty() && !signing.travelled().contains("released"),
        "nothing was released: {}",
        signing.travelled()
    );
    signing.hosted.page.peer.close();
}

/// A window is answered for the item its fill names and no other: one that asks for another
/// item of the same person and the same site is refused, and nobody is asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_window_that_asks_for_another_item_than_its_fill_names_is_refused() {
    let mut signing = Box::pin(at_the_sign_in_page(SITE)).await;
    let other = signing
        .hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD,
            ),
            serde_json::json!({
                "kind": "login", "belongsTo": [SITE], "username": "ada-2",
                "secret": OTHER_SECRET,
            }),
        )
        .await;
    let other = other["reference"].as_str().expect("a reference").to_owned();
    signing.shown.strays.lock().unwrap().another_item = Some(other);
    let told = signing
        .asks_to_fill("Please sign in to the made-up site for me.", "call-1")
        .await;
    assert_eq!(
        signing.shown.handed.lock().unwrap().as_slice(),
        [FIELD],
        "the window was handed the fill: {told}"
    );
    assert!(
        !told.contains(ASKED) && !told.contains(SECRET) && !told.contains(OTHER_SECRET),
        "the agent is told nothing was filled: {told}"
    );
    assert!(
        signing.hosted.page.asks().await.is_empty(),
        "nobody is asked about an item no fill names"
    );
    assert!(
        signing.filled().is_empty()
            && signing.travelled().contains("Rejected")
            && !signing.travelled().contains("released"),
        "the ask is refused and nothing is released: {}",
        signing.travelled()
    );
    signing.hosted.page.peer.close();
}

/// An item of another site is refused where the page is: nobody is asked, nothing is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_of_another_site_is_refused_and_nobody_is_asked() {
    let mut signing = Box::pin(at_the_sign_in_page(ELSEWHERE)).await;
    let told = signing
        .asks_to_fill("Please sign in to the made-up site for me.", "call-1")
        .await;
    assert_eq!(
        signing.shown.handed.lock().unwrap().as_slice(),
        [FIELD],
        "the window was handed the fill and said where its page is: {told}"
    );
    assert!(
        !told.contains(ASKED) && !told.contains(SECRET),
        "the agent is told it was refused: {told}"
    );
    assert!(
        signing.hosted.page.asks().await.is_empty(),
        "nobody is asked about a site the item does not belong to"
    );
    assert!(
        signing.filled().is_empty() && !signing.travelled().contains("released"),
        "nothing was released: {}",
        signing.travelled()
    );
    signing.hosted.page.peer.close();
}

/// An agent its person narrowed, whose rules do not name the fill, is refused before any window
/// is asked: nothing is handed over, nobody is asked, nothing is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_whose_rules_do_not_name_the_fill_is_refused_before_any_window_is_asked() {
    let mut signing = Box::pin(at_the_sign_in_page(SITE)).await;
    // Everything of Chat, so it answers its person, and nothing of the vault.
    narrow(
        &signing.hosted,
        vec![Bound {
            node: format!("workspace:{}/app:{CHAT}", signing.hosted.workspace),
            kinds: vec!["read".into(), "undoable".into()],
            answer: "allow".into(),
        }],
    )
    .await;
    let told = signing
        .asks_to_fill("Please sign in to the made-up site for me.", "call-1")
        .await;
    assert!(
        signing.shown.handed.lock().unwrap().is_empty(),
        "no window is handed anything for it: {told}"
    );
    assert!(
        !told.contains(ASKED) && !told.contains(SECRET),
        "the agent is told it may not: {told}"
    );
    assert!(
        signing.hosted.page.asks().await.is_empty(),
        "nobody is asked"
    );
    assert!(signing.filled().is_empty() && signing.travelled().is_empty());
    signing.hosted.page.peer.close();
}
