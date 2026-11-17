//! A workspace whose person let its keys run commands unattended, on the relay
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
//! The person is a tilo.sh tab holding their root: the page's approver session
//! ([`tilo_server_client::questions`]) shows each question the relay asks them,
//! and the test answers it there, as they would.

use super::api::ISSUER;
use super::api_on_relay::{OnRelay, api_on_relay_shelled, approved_key, headers, json_of};
use super::*;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tilo_api::Server;
use tilo_server_client::MemberApprover;
use tilo_server_client::questions::{Answered, MemberQuestions, QuestionSheet, answer_questions};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tokio::sync::mpsc;

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
    async fn unattended(&self, workspace: &str, on: bool) -> (u16, Value) {
        self.send(
            hyper::Method::PUT,
            &format!("/v1/workspaces/{workspace}/unattended"),
            &json!({"on": on}),
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
        let (status, listed) = self.get("/v1/workspaces").await;
        assert_eq!(status, 200, "{listed}");
        let row = listed["workspaces"]
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
            .post("/v1/keys", &json!({"workspace": workspace, "name": name}))
            .await;
        assert_eq!(status, 201, "{minted}");
        minted["token"].as_str().unwrap().to_owned()
    }
}

/// What the page's sheet shows: every question the relay put to the person.
pub(super) struct Sheet {
    pub(super) asked: Arc<Mutex<Vec<PendingConfirmation>>>,
}

impl QuestionSheet for Sheet {
    fn ask(&mut self, question: &PendingConfirmation) {
        self.asked.lock().unwrap().push(question.clone());
    }

    fn withdraw(&mut self, _id: [u8; 32]) {}
}

/// The person at their page: what it was asked, and where their answers go.
pub(super) struct Person {
    pub(super) asked: Arc<Mutex<Vec<PendingConfirmation>>>,
    pub(super) answers: mpsc::UnboundedSender<Answered>,
}

impl Person {
    pub(super) fn questions(&self) -> usize {
        self.asked.lock().unwrap().len()
    }

    pub(super) fn last(&self) -> PendingConfirmation {
        self.asked.lock().unwrap().last().cloned().unwrap()
    }

    /// Answer the last question asked with `answer`.
    pub(super) fn answer(&self, answer: ConfirmationAnswer) {
        self.answers.send((self.last().id, answer)).unwrap();
    }
}

/// `call`, while the person's page keeps its session with the relay.
pub(super) async fn beside<T>(
    page: &mut Pin<&mut impl Future<Output = tilo_server_client::CertifyError>>,
    call: impl Future<Output = T>,
) -> T {
    tokio::select! {
        out = call => out,
        error = page.as_mut() => panic!("the page's session with the relay ended: {error}"),
    }
}

/// Wait, beside the page, until it was asked `count` questions in all.
pub(super) async fn asked(
    page: &mut Pin<&mut impl Future<Output = tilo_server_client::CertifyError>>,
    person: &Person,
    count: usize,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while person.questions() < count {
        assert!(
            Instant::now() < deadline,
            "the page saw {} questions, not {count}",
            person.questions()
        );
        beside(page, tokio::time::sleep(Duration::from_millis(50))).await;
    }
}

/// A person on `on_relay` with their page and a key reaching all their
/// workspaces, as their backend holds it.
pub(super) async fn backend(on_relay: &OnRelay) -> (SigningKey, String) {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&root, "Ada's browser", DeviceCapabilities::member());
    let token = Box::pin(approved_key(on_relay, &root, &page, "Visa backend")).await;
    (root, token)
}

/// A new workspace of the backend key's person named `name`: its id.
pub(super) async fn workspace(key: &Key<'_>, name: &str) -> String {
    let (status, made) = key.post("/v1/workspaces", &json!({"name": name})).await;
    assert_eq!(status, 201, "{made}");
    made["id"].as_str().unwrap().to_owned()
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

#[allow(
    clippy::large_futures,
    reason = "one story, polled once from the heap by its test"
)]
async fn own_key_runs_unasked_only_while_let() {
    let on_relay = Box::pin(api_on_relay_shelled(ISSUER)).await;
    let api = Arc::clone(&on_relay.api);
    let (ada, token) = backend(&on_relay).await;
    let backend = Key { api: &api, token };
    let ws = workspace(&backend, "Applicant: Jane Doe").await;
    let agent = Key {
        api: &api,
        token: backend.mint(&ws, "Jane Doe's agent").await,
    };

    // Ada's tab holds her root: it is her approver at the relay.
    let host = MemberApprover::new(on_relay.relay_host());
    let (answers, mut answered) = mpsc::unbounded_channel();
    let person = Person {
        asked: Arc::default(),
        answers,
    };
    let mut sheet = Sheet {
        asked: Arc::clone(&person.asked),
    };
    let mut questions = MemberQuestions {
        host: &host,
        root: &ada,
        device: None,
    };
    let page = answer_questions(&mut questions, &mut sheet, &mut answered, || {
        tokio::time::sleep(Duration::from_millis(50))
    });
    tokio::pin!(page);

    // Off, as every workspace starts: the workspace's own key's command is put
    // to the person.
    assert!(!beside(&mut page, agent.listed_unattended(&ws)).await);
    let (status, ran) = beside(&mut page, agent.run(&ws, "echo asked-while-off")).await;
    assert!(
        waiting(status, &ran).is_some(),
        "asked while off: {status} {ran}"
    );
    asked(&mut page, &person, 1).await;
    person.answer(ConfirmationAnswer::Deny);

    // The key asks to run unattended: pending while the person decides, and
    // the question names the workspace and its branches.
    let (status, asking) = beside(&mut page, agent.unattended(&ws, true)).await;
    let run = waiting(status, &asking).unwrap_or_else(|| panic!("pending: {status} {asking}"));
    assert!(asking["ask"].is_object(), "an ask the SDKs raise: {asking}");
    asked(&mut page, &person, 2).await;
    let question = person.last();
    assert_eq!(question.agent_label, "Jane Doe's agent", "{question:?}");
    assert_eq!(question.workspace, "Applicant: Jane Doe", "{question:?}");
    assert!(
        question.resource_label.contains("branches made from it")
            && question
                .action_label
                .contains("run commands in its Linux machine without asking you each time"),
        "{question:?}"
    );
    // Denied, it stays off, and the next command is asked.
    person.answer(ConfirmationAnswer::Deny);
    let (status, denied) = beside(&mut page, agent.follow(&ws, &run)).await;
    assert!(
        status >= 400 && denied.to_string().contains("did not allow"),
        "denied: {status} {denied}"
    );
    assert!(!beside(&mut page, agent.listed_unattended(&ws)).await);
    let (status, ran) = beside(&mut page, agent.run(&ws, "echo asked-after-denial")).await;
    assert!(
        waiting(status, &ran).is_some(),
        "asked after the denial: {status} {ran}"
    );
    asked(&mut page, &person, 3).await;
    person.answer(ConfirmationAnswer::Deny);

    // Asked again and allowed: on.
    let (status, asking) = beside(&mut page, agent.unattended(&ws, true)).await;
    let run = waiting(status, &asking).unwrap_or_else(|| panic!("pending: {status} {asking}"));
    asked(&mut page, &person, 4).await;
    person.answer(ConfirmationAnswer::AllowOnce);
    let (status, allowed) = beside(&mut page, agent.follow(&ws, &run)).await;
    assert_eq!(status, 200, "{allowed}");
    assert_eq!(allowed["result"]["unattended"], true, "{allowed}");
    assert!(beside(&mut page, agent.listed_unattended(&ws)).await);
    assert!(beside(&mut page, backend.listed_unattended(&ws)).await);

    // On: the workspace's own key's command runs, nobody asked, and History
    // keeps it as that key's run.
    let (status, ran) = beside(&mut page, agent.run(&ws, "echo ran-unattended")).await;
    assert_eq!(status, 200, "ran unasked: {ran}");
    assert!(ran.to_string().contains("ran-unattended"), "{ran}");
    assert_eq!(person.questions(), 4, "nobody was asked");
    let (status, history) = beside(
        &mut page,
        agent.post(
            &format!("/v1/workspaces/{ws}/resources/terminal/actions/history"),
            &json!({"last": 100}),
        ),
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
    let (status, ran) = beside(&mut page, backend.run(&ws, "echo backend-asked")).await;
    assert!(
        waiting(status, &ran).is_some(),
        "the backend's key is asked: {status} {ran}"
    );
    asked(&mut page, &person, 5).await;
    person.answer(ConfirmationAnswer::Deny);

    // Another action of the key's that reaches outside the records still
    // asks: running a recorded command again goes to the session's pane,
    // which need not be the workspace's machine.
    let recorded = recorded_run(&mut page, &agent, &ws).await;
    let (status, rerun) = beside(
        &mut page,
        agent.post(
            &format!("/v1/workspaces/{ws}/resources/{}/actions/rerun", recorded.0),
            &json!({"runs": recorded.1}),
        ),
    )
    .await;
    assert!(
        waiting(status, &rerun).is_some(),
        "a rerun is asked: {status} {rerun}"
    );
    asked(&mut page, &person, 6).await;
    person.answer(ConfirmationAnswer::Deny);

    // Off again, at once, asking nobody; the next command is asked.
    let (status, off) = beside(&mut page, agent.unattended(&ws, false)).await;
    assert_eq!(status, 200, "{off}");
    assert_eq!(off["result"]["unattended"], false, "{off}");
    assert_eq!(person.questions(), 6, "turning it off asks nobody");
    assert!(!beside(&mut page, agent.listed_unattended(&ws)).await);
    let (status, ran) = beside(&mut page, agent.run(&ws, "echo asked-again")).await;
    assert!(
        waiting(status, &ran).is_some(),
        "asked once off again: {status} {ran}"
    );
    asked(&mut page, &person, 7).await;
    person.answer(ConfirmationAnswer::Deny);
    beside(&mut page, tokio::time::sleep(Duration::from_millis(200))).await;
    host.close().await;
}

/// The Terminal session of `workspace` and the id of a run it recorded, as
/// `key` reads them.
async fn recorded_run(
    page: &mut Pin<&mut impl Future<Output = tilo_server_client::CertifyError>>,
    key: &Key<'_>,
    workspace: &str,
) -> (String, String) {
    let (status, listed) = beside(
        page,
        key.get(&format!(
            "/v1/workspaces/{workspace}/resources?path=terminal"
        )),
    )
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
        let (status, read) = beside(
            page,
            key.get(&format!("/v1/workspaces/{workspace}/resources/{encoded}")),
        )
        .await;
        assert_eq!(status, 200, "{read}");
        let run = read["fields"]["runs"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|run| run["run"].as_str().or_else(|| run["id"].as_str()));
        if let Some(run) = run {
            return (encoded, run.to_owned());
        }
    }
    panic!("no Terminal session of {workspace} recorded a run: {listed}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_branch_runs_its_own_keys_commands_unasked_and_goes_on_apart() {
    Box::pin(branch_runs_unasked_and_goes_on_apart()).await;
}

#[allow(
    clippy::large_futures,
    reason = "one story, polled once from the heap by its test"
)]
async fn branch_runs_unasked_and_goes_on_apart() {
    let on_relay = Box::pin(api_on_relay_shelled(ISSUER)).await;
    let api = Arc::clone(&on_relay.api);
    let (ada, token) = backend(&on_relay).await;
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

    let host = MemberApprover::new(on_relay.relay_host());
    let (answers, mut answered) = mpsc::unbounded_channel();
    let person = Person {
        asked: Arc::default(),
        answers,
    };
    let mut sheet = Sheet {
        asked: Arc::clone(&person.asked),
    };
    let mut questions = MemberQuestions {
        host: &host,
        root: &ada,
        device: None,
    };
    let page = answer_questions(&mut questions, &mut sheet, &mut answered, || {
        tokio::time::sleep(Duration::from_millis(50))
    });
    tokio::pin!(page);

    // The person lets the template run its keys' commands unattended, asked
    // by the backend's key.
    let (status, asking) = beside(&mut page, backend.unattended(&template, true)).await;
    let run = waiting(status, &asking).unwrap_or_else(|| panic!("pending: {status} {asking}"));
    asked(&mut page, &person, 1).await;
    person.answer(ConfirmationAnswer::AllowOnce);
    let (status, allowed) = beside(&mut page, backend.follow(&template, &run)).await;
    assert_eq!(status, 200, "{allowed}");

    // A key for another workspace neither turns it on there nor uses it here.
    let (status, refused) = beside(&mut page, others.unattended(&template, true)).await;
    assert!(
        status >= 400,
        "a key for another workspace: {status} {refused}"
    );
    let (status, refused) = beside(&mut page, others.run(&template, "echo not-mine")).await;
    assert!(
        status >= 400,
        "a key for another workspace runs nothing: {status} {refused}"
    );
    let (status, ran) = beside(&mut page, others.run(&other, "echo asked-in-its-own")).await;
    assert!(
        waiting(status, &ran).is_some(),
        "the other workspace's key is asked in its own: {status} {ran}"
    );
    asked(&mut page, &person, 2).await;
    person.answer(ConfirmationAnswer::Deny);
    assert!(!beside(&mut page, others.listed_unattended(&other)).await);

    // A branch of the template starts with it on: a key minted for the
    // branch runs its commands there unasked.
    let (status, branched) = beside(
        &mut page,
        backend.post(
            &format!("/v1/workspaces/{template}/branches"),
            &json!({"name": "Applicant: Jane Doe"}),
        ),
    )
    .await;
    assert_eq!(status, 201, "{branched}");
    let branch = branched["id"].as_str().unwrap().to_owned();
    assert_eq!(branched["unattended"], true, "{branched}");
    let branchs = Key {
        api: &api,
        token: backend.mint(&branch, "Jane Doe's agent").await,
    };
    assert!(beside(&mut page, branchs.listed_unattended(&branch)).await);
    let (status, ran) = beside(&mut page, branchs.run(&branch, "echo branch-unasked")).await;
    assert_eq!(status, 200, "the branch's key runs unasked: {ran}");
    assert!(ran.to_string().contains("branch-unasked"), "{ran}");
    assert_eq!(person.questions(), 2, "nobody was asked");

    // The template's key does not reach the branch.
    let (status, refused) = beside(&mut page, templates.run(&branch, "echo not-mine")).await;
    assert!(
        status >= 400,
        "the template's key runs nothing in the branch: {status} {refused}"
    );

    // Off in the branch, which asks again; the template's stays on.
    let (status, off) = beside(&mut page, branchs.unattended(&branch, false)).await;
    assert_eq!(status, 200, "{off}");
    let (status, ran) = beside(&mut page, branchs.run(&branch, "echo branch-asked")).await;
    assert!(
        waiting(status, &ran).is_some(),
        "asked in the branch: {status} {ran}"
    );
    asked(&mut page, &person, 3).await;
    person.answer(ConfirmationAnswer::Deny);
    assert!(beside(&mut page, templates.listed_unattended(&template)).await);
    let (status, ran) = beside(&mut page, templates.run(&template, "echo template-unasked")).await;
    assert_eq!(
        status, 200,
        "the template's own key still runs unasked: {ran}"
    );
    assert_eq!(person.questions(), 3);
    beside(&mut page, tokio::time::sleep(Duration::from_millis(200))).await;
    host.close().await;
}
