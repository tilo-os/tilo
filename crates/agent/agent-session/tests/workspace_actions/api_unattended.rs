//! A workspace whose person let its keys run commands unattended, on the server
//! as tilo.sh hosts it, through the API: each command a key runs in a
//! workspace's machine (`terminal#run`) is put to the person, until they allow,
//! on their approving device, that the keys made for that workspace alone run
//! commands there without asking. A key asks for it and is answered pending;
//! denied, nothing changes; allowed, the workspace's own key runs its commands
//! unasked, as its run in History, while a key reaching all the person's
//! workspaces and every other action that asked still ask. Off again, the next
//! command is asked. A branch starts with its source's setting, and goes on
//! apart from it; a key for one workspace neither turns on nor uses another's.
//!
//! The person is a page of their own in the workspace: the questions that wait for them are
//! asked records they read there, and their answer is the record's `resolve`.

use super::api::ISSUER;
use super::api_on_server::{OnServer, api_on_server_shelled, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_actions::remote::asked::AskedBy;
use tilo_api::Server;

/// A key's calls of `/v1`.
pub(super) struct Key<'a> {
    pub(super) api: &'a Arc<Server>,
    pub(super) token: String,
}

impl Key<'_> {
    pub(super) async fn send(
        &self,
        method: hyper::Method,
        path: &str,
        body: &Value,
    ) -> (u16, Value) {
        let bearer = format!("Bearer {}", self.token);
        let pairs = [
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ];
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let body = if body.is_null() {
            String::new()
        } else {
            body.to_string()
        };
        // The API's answer is a large future: on the heap, not in the frames
        // of the story that awaits it.
        let reply =
            Box::pin(
                self.api
                    .dispatch(&method, path, query, &headers(&pairs), body.as_bytes()),
            )
            .await;
        Box::pin(json_of(reply)).await
    }

    pub(super) async fn get(&self, path: &str) -> (u16, Value) {
        self.send(hyper::Method::GET, path, &Value::Null).await
    }

    pub(super) async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        self.send(hyper::Method::POST, path, body).await
    }

    /// Run `command` in `workspace`'s Terminal.
    pub(super) async fn run(&self, workspace: &str, command: &str) -> (u16, Value) {
        self.post(
            &format!("/v1/workspaces/{workspace}/resources/terminal/actions/run"),
            &json!({"command": command}),
        )
        .await
    }

    /// Turn running commands unattended in `workspace` on or off.
    pub(super) async fn unattended(&self, workspace: &str, on: bool) -> (u16, Value) {
        self.post(
            &format!("/v1/workspaces/{workspace}/resources/account/workspaces/actions/unattended"),
            &json!({"workspace": workspace, "on": on}),
        )
        .await
    }

    /// Follow `run` in `workspace` until it is no longer pending.
    pub(super) async fn follow(&self, workspace: &str, run: &str) -> (u16, Value) {
        self.get(&format!(
            "/v1/workspaces/{workspace}/invocations/{run}?wait=30"
        ))
        .await
    }

    /// Whether `workspace`'s listing says it runs commands unattended.
    async fn listed_unattended(&self, workspace: &str) -> bool {
        let (status, listed) = self
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(status, 200, "{listed}");
        let row = listed["result"]["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["id"] == workspace)
            .unwrap_or_else(|| panic!("{workspace} is listed: {listed}"));
        row["unattended"] == true
    }

    /// A key reaching `workspace` alone, named `name`: its token.
    pub(super) async fn mint(&self, workspace: &str, name: &str) -> String {
        let (status, minted) = self
            .post(
                "/v1/resources/account/keys/actions/mint",
                &json!({"workspace": workspace, "name": name}),
            )
            .await;
        assert_eq!(status, 200, "{minted}");
        minted["result"]["token"].as_str().unwrap().to_owned()
    }
}

/// The person `root`'s own page in `workspace`, on `on_server`'s host.
pub(super) async fn page_in(on_server: &OnServer, root: &SigningKey, workspace: &str) -> Browser {
    let device = enrol(root, "Ada's browser", DeviceCapabilities::member());
    Browser::open_at(
        &on_server.hosted.server,
        &device,
        &on_server.host,
        workspace,
    )
    .await
}

/// The person answers the question the invocation `invocation` waits in at `page`, with
/// `resolution`: `approve`, `approve_and_grant` or `decline`.
pub(super) async fn answer(page: &mut Browser, invocation: &str, resolution: &str) {
    let asked = page
        .asked_about(invocation.parse().expect("an invocation id"))
        .await;
    page.resolve(&asked, resolution, None)
        .await
        .unwrap_or_else(|why| panic!("the person answers {invocation}: {why}"));
}

/// Whether nothing waits for the person, as `page` lists their questions.
pub(super) async fn nothing_waits(page: &mut Browser) -> bool {
    page.asks().await.is_empty()
}

/// A person on `on_server` with their page and a key reaching all their
/// workspaces, as their backend holds it.
pub(super) async fn backend(on_server: &OnServer) -> (SigningKey, String) {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&root, "Ada's browser", DeviceCapabilities::member());
    let token = Box::pin(approved_key(on_server, &root, &page, "Visa backend")).await;
    (root, token)
}

/// A new workspace of the backend key's person named `name`: its id.
pub(super) async fn workspace(key: &Key<'_>, name: &str) -> String {
    let (status, made) = key
        .post(
            "/v1/resources/account/workspaces/actions/create",
            &json!({"name": name}),
        )
        .await;
    assert_eq!(status, 200, "{made}");
    made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Whether `answer` is a run still pending, as one waiting for the person
/// is: its run.
pub(super) fn waiting(status: u16, answer: &Value) -> Option<String> {
    (status == 202 && answer["status"] == "pending")
        .then(|| answer["invocation"].as_str().unwrap().to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspaces_own_key_runs_commands_unasked_only_while_its_person_lets_it() {
    Box::pin(own_key_runs_unasked_only_while_let()).await;
}

async fn own_key_runs_unasked_only_while_let() {
    let on_server = Box::pin(api_on_server_shelled(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (ada, token) = backend(&on_server).await;
    let backend = Key { api: &api, token };
    let ws = workspace(&backend, "Applicant: Jane Doe").await;
    let agent = Key {
        api: &api,
        token: backend.mint(&ws, "Jane Doe's agent").await,
    };

    let mut person = page_in(&on_server, &ada, &ws).await;

    // Off, as every workspace starts: the workspace's own key's command is put
    // to the person.
    assert!(!agent.listed_unattended(&ws).await);
    let (status, ran) = agent.run(&ws, "echo asked-while-off").await;
    let asked = waiting(status, &ran).unwrap_or_else(|| panic!("asked while off: {status} {ran}"));
    answer(&mut person, &asked, "decline").await;

    // The key asks to run unattended: it waits for the person, and the question
    // names the workspace and the action.
    let (status, asking) = agent.unattended(&ws, true).await;
    let run = waiting(status, &asking).unwrap_or_else(|| panic!("pending: {status} {asking}"));
    assert!(asking["ask"].is_object(), "an ask the SDKs raise: {asking}");
    let question = person
        .asked_about(run.parse().expect("an invocation id"))
        .await;
    let AskedBy::Agent(who) = &question.by else {
        panic!("an agent asks: {question:?}");
    };
    assert_eq!(who.name, "Jane Doe's agent", "{question:?}");
    assert_eq!(
        question.workspace_name, "Applicant: Jane Doe",
        "{question:?}"
    );
    assert!(
        question
            .name
            .contains("run commands in its Linux machine without asking you each time"),
        "{question:?}"
    );
    // Denied, it stays off, and the next command is asked.
    answer(&mut person, &run, "decline").await;
    let (status, denied) = agent.follow(&ws, &run).await;
    assert!(
        status >= 400 && denied.to_string().contains("did not allow"),
        "denied: {status} {denied}"
    );
    assert!(!agent.listed_unattended(&ws).await);
    let (status, ran) = agent.run(&ws, "echo asked-after-denial").await;
    let asked =
        waiting(status, &ran).unwrap_or_else(|| panic!("asked after the denial: {status} {ran}"));
    answer(&mut person, &asked, "decline").await;

    // Asked again and allowed: on.
    let (status, asking) = agent.unattended(&ws, true).await;
    let run = waiting(status, &asking).unwrap_or_else(|| panic!("pending: {status} {asking}"));
    answer(&mut person, &run, "approve").await;
    let (status, allowed) = agent.follow(&ws, &run).await;
    assert_eq!(status, 200, "{allowed}");
    assert_eq!(allowed["result"]["unattended"], true, "{allowed}");
    assert!(agent.listed_unattended(&ws).await);
    assert!(backend.listed_unattended(&ws).await);

    // On: the workspace's own key's command runs, nobody asked, and History
    // keeps it as that key's run.
    let (status, ran) = agent.run(&ws, "echo ran-unattended").await;
    assert_eq!(status, 200, "ran unasked: {ran}");
    assert!(ran.to_string().contains("ran-unattended"), "{ran}");
    assert!(nothing_waits(&mut person).await, "nobody was asked");
    let (status, history) = agent
        .post(
            &format!("/v1/workspaces/{ws}/resources/terminal/actions/history"),
            &json!({"last": 100}),
        )
        .await;
    assert_eq!(status, 200, "{history}");
    let by_key = history["result"]["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|version| {
            version["actor"]["name"] == "Jane Doe's agent"
                && version["actor"]["by"]["member"]["agent"].is_string()
        })
        .count();
    assert!(by_key > 0, "History names the run the key's: {history}");

    // The key reaching all the person's workspaces is still asked there.
    let (status, ran) = backend.run(&ws, "echo backend-asked").await;
    let asked = waiting(status, &ran)
        .unwrap_or_else(|| panic!("the backend's key is asked: {status} {ran}"));
    answer(&mut person, &asked, "decline").await;

    // Another action of the key's that reaches outside the records still
    // asks: running a recorded command again goes to the session's pane,
    // which need not be the workspace's machine.
    let recorded = recorded_command(&agent, &ws).await;
    let (status, rerun) = agent
        .post(
            &format!("/v1/workspaces/{ws}/resources/{}/actions/rerun", recorded.0),
            &json!({"command": recorded.1}),
        )
        .await;
    let asked =
        waiting(status, &rerun).unwrap_or_else(|| panic!("a rerun is asked: {status} {rerun}"));
    answer(&mut person, &asked, "decline").await;

    // Off again, at once, asking nobody; the next command is asked.
    let (status, off) = agent.unattended(&ws, false).await;
    assert_eq!(status, 200, "{off}");
    assert_eq!(off["result"]["unattended"], false, "{off}");
    assert!(
        nothing_waits(&mut person).await,
        "turning it off asks nobody"
    );
    assert!(!agent.listed_unattended(&ws).await);
    let (status, ran) = agent.run(&ws, "echo asked-again").await;
    let asked =
        waiting(status, &ran).unwrap_or_else(|| panic!("asked once off again: {status} {ran}"));
    answer(&mut person, &asked, "decline").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    person.peer.close();
}

/// The Terminal session of `workspace` and the id of a command it ran, as
/// `key` reads them.
async fn recorded_command(key: &Key<'_>, workspace: &str) -> (String, String) {
    let (status, listed) = key
        .get(&format!(
            "/v1/workspaces/{workspace}/resources?path=terminal"
        ))
        .await;
    assert_eq!(status, 200, "{listed}");
    for row in listed["rows"].as_array().into_iter().flatten() {
        let Some(path) = row["path"].as_str() else {
            continue;
        };
        let encoded = path
            .split('/')
            .map(|segment| segment.replace(' ', "%20"))
            .collect::<Vec<_>>()
            .join("/");
        let (status, read) = key
            .get(&format!("/v1/workspaces/{workspace}/resources/{encoded}"))
            .await;
        assert_eq!(status, 200, "{read}");
        // A command's entry is its id, which is the run's.
        let command = read["fields"]["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|entry| entry["id"].as_str());
        if let Some(command) = command {
            return (encoded, command.to_owned());
        }
    }
    panic!("no Terminal session of {workspace} ran a command: {listed}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_branch_runs_its_own_keys_commands_unasked_and_goes_on_apart() {
    Box::pin(branch_runs_unasked_and_goes_on_apart()).await;
}

async fn branch_runs_unasked_and_goes_on_apart() {
    let on_server = Box::pin(api_on_server_shelled(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (ada, token) = backend(&on_server).await;
    let backend = Key { api: &api, token };
    let template = workspace(&backend, "Visa template").await;
    let other = workspace(&backend, "Another applicant").await;
    let templates = Key {
        api: &api,
        token: backend.mint(&template, "Template's agent").await,
    };
    let others = Key {
        api: &api,
        token: backend.mint(&other, "Other's agent").await,
    };

    let mut person = page_in(&on_server, &ada, &template).await;

    // The person lets the template run its keys' commands unattended, asked
    // by the backend's key.
    let (status, asking) = backend.unattended(&template, true).await;
    let run = waiting(status, &asking).unwrap_or_else(|| panic!("pending: {status} {asking}"));
    answer(&mut person, &run, "approve").await;
    let (status, allowed) = backend.follow(&template, &run).await;
    assert_eq!(status, 200, "{allowed}");

    // A key for another workspace neither turns it on there nor uses it here.
    let (status, refused) = others.unattended(&template, true).await;
    assert!(
        status >= 400,
        "a key for another workspace: {status} {refused}"
    );
    let (status, refused) = others.run(&template, "echo not-mine").await;
    assert!(
        status >= 400,
        "a key for another workspace runs nothing: {status} {refused}"
    );
    let (status, ran) = others.run(&other, "echo asked-in-its-own").await;
    let asked = waiting(status, &ran)
        .unwrap_or_else(|| panic!("the other workspace's key is asked in its own: {status} {ran}"));
    let mut at_other = page_in(&on_server, &ada, &other).await;
    answer(&mut at_other, &asked, "decline").await;
    assert!(!others.listed_unattended(&other).await);

    // A branch of the template starts with it on: a key minted for the
    // branch runs its commands there unasked.
    let (status, branched) = backend
        .post(
            "/v1/resources/account/workspaces/actions/branch",
            &json!({"workspace": template, "name": "Applicant: Jane Doe"}),
        )
        .await;
    assert_eq!(status, 200, "{branched}");
    let branched = &branched["result"]["workspace"];
    let branch = branched["id"].as_str().unwrap().to_owned();
    assert_eq!(branched["unattended"], true, "{branched}");
    let branchs = Key {
        api: &api,
        token: backend.mint(&branch, "Jane Doe's agent").await,
    };
    assert!(branchs.listed_unattended(&branch).await);
    let (status, ran) = branchs.run(&branch, "echo branch-unasked").await;
    assert_eq!(status, 200, "the branch's key runs unasked: {ran}");
    assert!(ran.to_string().contains("branch-unasked"), "{ran}");
    assert!(nothing_waits(&mut person).await, "nobody was asked");

    // The template's key does not reach the branch.
    let (status, refused) = templates.run(&branch, "echo not-mine").await;
    assert!(
        status >= 400,
        "the template's key runs nothing in the branch: {status} {refused}"
    );

    // Off in the branch, which asks again; the template's stays on.
    let (status, off) = branchs.unattended(&branch, false).await;
    assert_eq!(status, 200, "{off}");
    let (status, ran) = branchs.run(&branch, "echo branch-asked").await;
    let asked =
        waiting(status, &ran).unwrap_or_else(|| panic!("asked in the branch: {status} {ran}"));
    let mut at_branch = page_in(&on_server, &ada, &branch).await;
    answer(&mut at_branch, &asked, "decline").await;
    assert!(templates.listed_unattended(&template).await);
    let (status, ran) = templates.run(&template, "echo template-unasked").await;
    assert_eq!(
        status, 200,
        "the template's own key still runs unasked: {ran}"
    );
    assert!(nothing_waits(&mut person).await);
    tokio::time::sleep(Duration::from_millis(200)).await;
    for mut page in [person, at_other, at_branch] {
        page.peer.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keys_terminal_open_is_asked_once_and_its_stream_is_its_device_alone() {
    Box::pin(terminal_open_is_asked_once()).await;
}

/// A key opening the workspace's Terminal on a terminal is asked about as its commands are:
/// once for the open, the stream it answers carrying the keys without asking again. The stream
/// is the key's device's: another device presenting it at the server is refused there.
async fn terminal_open_is_asked_once() {
    let on_server = Box::pin(api_on_server_shelled(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (ada, token) = backend(&on_server).await;
    let backend = Key { api: &api, token };
    let ws = workspace(&backend, "Applicant: Jane Doe").await;
    let agent = Key {
        api: &api,
        token: backend.mint(&ws, "Jane Doe's agent").await,
    };
    let mut person = page_in(&on_server, &ada, &ws).await;
    let (path, body) = (
        format!("/v1/workspaces/{ws}/resources/terminal/actions/open-terminal"),
        json!({"cols": 100, "rows": 30}),
    );
    let open = || agent.post(&path, &body);

    // Asked first, as a command of the key's is; allowed, it opens.
    let (status, asking) = open().await;
    let opening = waiting(status, &asking).unwrap_or_else(|| panic!("asked: {status} {asking}"));
    answer(&mut person, &opening, "approve").await;
    let (status, opened) = agent.follow(&ws, &opening).await;
    assert_eq!(status, 200, "{opened}");
    let is_id = |id: &Value| {
        id.as_str()
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    assert!(
        is_id(&opened["result"]["stream"]) && is_id(&opened["result"]["session"]),
        "a stream and a session: {opened}"
    );

    // The server serves the stream to the key's device alone.
    let (grace, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let phone = enrol(&grace, "phone", DeviceCapabilities::member());
    let endpoint = test_server::endpoint(iroh::SecretKey::from_bytes(&phone.key.to_bytes())).await;
    let server = on_server.hosted.server.node_addr();
    let stream = opened["result"]["stream"].as_str().unwrap().to_owned();
    let welcome = tokio::task::spawn_blocking(move || {
        use ush::environment_terminals::{quic, wire};
        let runtime = tokio::runtime::Handle::current();
        let (_connection, mut to, mut from) = quic::dial(&endpoint, server, &runtime).unwrap();
        wire::write_hello(&mut to, &wire::Hello { stream }).unwrap();
        wire::read_welcome(&mut from).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        welcome,
        Some(ush::environment_terminals::wire::Welcome::Refused {
            reason: ush::environment_terminals::NOT_HANDED.into()
        }),
        "another device is refused the key's stream"
    );

    // Each open is asked about: the allowed one let only itself through.
    let (status, again) = open().await;
    let asked = waiting(status, &again)
        .unwrap_or_else(|| panic!("the next open is asked: {status} {again}"));
    answer(&mut person, &asked, "decline").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    person.peer.close();
}
