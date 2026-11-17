//! A turn of the assistant sends its workspace's Browser window to a web address, with no
//! device of its person doing anything: to a public address, and to no other.
//!
//! The server, the person and the assistant are those of the test this is a part of. The
//! window is a stand-in the test holds: the device the server placed and certified for it
//! joins the workspace as the session in a workspace's machine does, keeps every page action
//! it is handed with who handed it, and answers as a window whose page opened. No engine
//! starts and no address is reached.
use super::*;
use tilo_actions::ActionOutput;
use tilo_actions::remote::{
    InvocationReceipt, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
};
use tilo_server::Envelope;
use tilo_server::hosted_browser::{BrowserSessions, HostedBrowsers, Unmetered, Window};
use tilo_server_protocol::hosted::Material;

/// The provider the stand-in window publishes under.
const WINDOW: &str = "4711";
/// What a turn is told of an address a window is not sent to.
const NOT_PUBLIC: &str = "goes to public web addresses only";
/// What it is told became of the window then.
const NOT_SENT: &str = "The window was not sent anywhere";

/// Where the server starts a workspace's Browser: nowhere, here. It keeps what the server
/// handed each window's session, for the stand-in to join as.
#[derive(Clone, Default)]
struct Machines(Arc<Mutex<Vec<Material>>>);

impl Machines {
    fn started(&self) -> Vec<Material> {
        self.0.lock().unwrap().clone()
    }
}

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

/// One page action the stand-in window was handed.
#[derive(Clone, Debug)]
struct Handed {
    /// The device that handed it.
    from: [u8; 32],
    /// The person it was handed for.
    person: [u8; 32],
    resource: String,
    action: String,
    arguments: serde_json::Value,
    /// Whether it came with anybody's answer for it.
    confirmed: bool,
}

/// Everything the stand-in window was handed, in order.
#[derive(Clone, Default)]
struct Shown(Arc<Mutex<Vec<Handed>>>);

impl Shown {
    fn handed(&self) -> Vec<Handed> {
        self.0.lock().unwrap().clone()
    }
}

/// A server that places Browser windows, with its person, their assistant and their model.
async fn placing() -> (Hosted, Machines) {
    let machines = Machines::default();
    let browsers = Arc::new(HostedBrowsers::new(
        Box::new(machines.clone()),
        Box::new(Unmetered),
        "acct_ada".into(),
    ));
    let hosted = Box::pin(Hosted::start_placing(Some(browsers))).await;
    (hosted, machines)
}

/// The model's step that opens Browser through the agent's tools.
fn opens(call: &str) -> Reply {
    does(call, "browser#open", &serde_json::json!({}))
}

/// The model's step that sends the window's page to `address` through the agent's tools.
fn goes(call: &str, address: &str) -> Reply {
    does(
        call,
        "browser/page#navigate",
        &serde_json::json!({"url": address}),
    )
}

/// The session that holds the workspace's Browser window, stood in for: the device the server
/// placed joins the workspace. Handed a page action, it keeps it and answers that its page is
/// at the address the action named.
async fn window_joins(hosted: &Hosted, material: &Material) -> Shown {
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
    // joins, and so does its stand-in: the workspace's governance then names the device.
    tilo_server::workspaces::enter_invitation(&client, &material.workspace, 600)
        .await
        .expect("the server enters the device it placed into its workspace");
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    loop {
        match tilo_test_env::within("the window joins", client.recv()).await {
            Some(Envelope::Ready { .. }) => break,
            Some(_) => {}
            None => panic!("the window's device was let go before it joined"),
        }
    }
    let window = material.device.device_public_key;
    let held = shown.clone();
    tokio::spawn(async move {
        while let Some(envelope) = client.recv().await {
            let Envelope::RemoteAction {
                from_device_public_key,
                from_user_public_key,
                frame,
                ..
            } = envelope
            else {
                continue;
            };
            let Ok(RemoteActionFrame { message, .. }) = RemoteActionFrame::decode(&frame) else {
                continue;
            };
            let RemoteActionMessage::Invoke(asked) = message else {
                continue;
            };
            let arguments = match &asked.argument {
                InvokeArgument::Json(arguments) => serde_json::Value::Object(arguments.clone()),
                _ => serde_json::Value::Null,
            };
            let at = arguments
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            held.0.lock().unwrap().push(Handed {
                from: from_device_public_key,
                person: from_user_public_key,
                resource: asked.target.address.resource.clone(),
                action: asked.target.address.action.clone(),
                arguments,
                confirmed: asked.confirmation.is_some(),
            });
            let answer = RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: asked.invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(serde_json::json!({
                            "url": at,
                            "message": "Opened the page",
                            "navigation": "started",
                        })),
                        resources: Vec::new(),
                    },
                },
            });
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

/// A server whose assistant opened Browser in a turn, with the stand-in window joined: the
/// server, what it handed the window's session, and what the window is handed from now on.
async fn with_a_window() -> (Hosted, Material, Shown) {
    let (hosted, machines) = Box::pin(placing()).await;
    Box::pin(hosted.turn(
        "Open the browser.",
        vec![opens("open-1"), Reply::text("Browser is open.")],
    ))
    .await;
    let material = machines.started().first().cloned().unwrap_or_else(|| {
        panic!(
            "the turn's open started no session: {}",
            hosted.last_tool_result()
        )
    });
    let shown = Box::pin(window_joins(&hosted, &material)).await;
    (hosted, material, shown)
}

/// The assistant's navigation, in a turn the server answers with no device of its person on,
/// reaches the workspace's window once: the address and nothing else, handed by the server's
/// own device for the person the turn works for, with nobody's answer attached.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_sends_its_workspaces_window_to_a_public_address() {
    let (hosted, material, shown) = Box::pin(with_a_window()).await;
    let address = "https://example.org/prices?year=2026";
    Box::pin(hosted.turn(
        "Look at the prices.",
        vec![goes("go-1", address), Reply::text("I am on the page.")],
    ))
    .await;
    let told = hosted.last_tool_result();
    let handed = shown.handed();
    assert_eq!(
        handed.len(),
        1,
        "the window is handed the one navigation: {told}"
    );
    let went = &handed[0];
    assert_eq!(went.action, "navigate", "{told}");
    assert_eq!(went.resource, "browser.page", "{told}");
    assert_eq!(
        went.arguments,
        serde_json::json!({"url": address}),
        "the address and nothing else"
    );
    assert!(
        !went.confirmed,
        "nobody's answer is attached to a navigation"
    );
    assert_eq!(
        hex::encode(went.person),
        hosted.person(),
        "for the person the turn works for"
    );
    assert_eq!(
        hex::encode(went.from),
        hex::encode(material.server.id.as_bytes()),
        "by the server's own device"
    );
    assert_ne!(
        hex::encode(went.from),
        hex::encode(material.device.device_public_key),
        "never by the device the server placed for the window"
    );
    assert!(
        told.contains("went_to") && told.contains(address),
        "the turn is told where the window went: {told}"
    );
    hosted.page.peer.close();
}

/// An address on the server's own machine, in a private range, on a network's inner names, or
/// of no web scheme is refused in words that say so, and the window is handed nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_is_refused_an_address_that_is_not_public_in_words() {
    let (hosted, _material, shown) = Box::pin(with_a_window()).await;
    for (at, address) in [
        "http://127.0.0.1:8080/admin",
        "http://10.0.0.5/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]:9000/",
        "http://server.internal/status",
        "file:///etc/passwd",
    ]
    .into_iter()
    .enumerate()
    {
        Box::pin(hosted.turn(
            &format!("Open {address}."),
            vec![
                goes(&format!("go-{at}"), address),
                Reply::text("I may not go there."),
            ],
        ))
        .await;
        let told = hosted.last_tool_result();
        assert!(
            told.contains(NOT_PUBLIC) && told.contains(NOT_SENT),
            "{address} is refused in words that say why: {told}"
        );
        assert!(
            shown.handed().is_empty(),
            "the window is handed nothing for {address}: {told}"
        );
    }
    hosted.page.peer.close();
}
