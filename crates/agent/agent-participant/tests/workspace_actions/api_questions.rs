//! What a person's agent asks them through the API server on a relay, as a
//! tilo.sh tab holding the person's root answers it: the page's approver
//! session ([`tilo_relay_client::questions`]), each command signed by the
//! root, as the browser runtime runs it. Allowed, the agent's call runs;
//! refused, it fails with the refusal; either way the question leaves the
//! sheet once the host stops asking it.
use super::api::{ISSUER, Mcp};
use super::api_on_relay::{api_on_relay, approved_key};
use super::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tilo_relay_client::MemberApprover;
use tilo_relay_client::questions::{Answered, MemberQuestions, QuestionSheet, answer_questions};
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tokio::sync::mpsc;

/// What the page's sheet saw: each question asked, and each taken away.
#[derive(Clone, Default)]
struct Seen {
    asked: Arc<Mutex<Vec<PendingConfirmation>>>,
    withdrawn: Arc<Mutex<Vec<[u8; 32]>>>,
}

/// The person at the sheet, answering every question with `answer`.
struct Person {
    answer: ConfirmationAnswer,
    answered: mpsc::UnboundedSender<Answered>,
    seen: Seen,
}

impl QuestionSheet for Person {
    fn ask(&mut self, question: &PendingConfirmation) {
        self.seen.asked.lock().unwrap().push(question.clone());
        let _ = self.answered.send((question.id, self.answer));
    }

    fn withdraw(&mut self, id: [u8; 32]) {
        self.seen.withdrawn.lock().unwrap().push(id);
    }
}

/// Removing a book from the library: Undo cannot give the reader back the
/// book's file, so the action asks first.
const REMOVE: &str = "ebook/1 book#remove-from-library";

/// The run the agent's call is a step of, by the title it gives it.
const RUN: &str = "Fill Jane Doe’s visa application";

/// `tilo_do` of [`REMOVE`] as an agent that waits for the person: the reply.
async fn asking(mcp: &mut Mcp<'_>) -> Value {
    let (status, reply) = mcp
        .post(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                     "params": {"name": "tilo_do",
                                "arguments": {"path": REMOVE, "args": {"book": "book:dusk"}},
                                "_meta": {"so.tilo.mcp": {"run": {"id": "visa-1",
                                                                  "title": RUN}}}}}))
        .await;
    assert_eq!(status, 200, "{reply}");
    reply
}

/// The agent's asks-first call, while the page holding `root`
/// answers what the relay at `host` asks with `answer`: the call's reply
/// and what the sheet saw, once the host stopped asking.
async fn answered_by_page(
    mcp: &mut Mcp<'_>,
    host: &MemberApprover,
    root: &SigningKey,
    answer: ConfirmationAnswer,
) -> (Value, Seen) {
    let seen = Seen::default();
    let (answered, mut answers) = mpsc::unbounded_channel();
    let mut person = Person {
        answer,
        answered,
        seen: seen.clone(),
    };
    let mut page = MemberQuestions { host, root };
    let session = answer_questions(&mut page, &mut person, &mut answers, || {
        tokio::time::sleep(Duration::from_millis(50))
    });
    tokio::pin!(session);
    let reply = tokio::select! {
        reply = asking(mcp) => reply,
        error = &mut session => panic!("the page's session with the relay ended: {error}"),
    };
    // The host stops asking a question once it is answered, and the sheet
    // lets it go.
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    while seen.withdrawn.lock().unwrap().len() < seen.asked.lock().unwrap().len() {
        assert!(Instant::now() < deadline, "the answered question stayed up");
        tokio::select! {
            error = &mut session => panic!("the page's session with the relay ended: {error}"),
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
    (reply, seen)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_holding_the_root_answers_what_the_agent_asks_through_the_door() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Visa backend").await;
    let mut account = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");
    let created = account
        .tilo_do(json!({"path": "account/workspaces#create",
                        "args": {"name": "Applicant: Jane Doe"}}))
        .await;
    let workspace = created["result"]["structuredContent"]["result"]["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a workspace is made: {created}"))
        .to_owned();
    let minted = account
        .tilo_do(json!({"path": "account/keys#mint",
                        "args": {"workspace": workspace, "name": "Jane Doe’s agent"}}))
        .await;
    let token = minted["result"]["structuredContent"]["result"]["token"]
        .as_str()
        .unwrap_or_else(|| panic!("a key is minted: {minted}"))
        .to_owned();
    let mut agent = Mcp {
        api: &api,
        path: format!("/w/{workspace}/mcp"),
        token,
        session: None,
    };
    let (status, init) = agent.initialize().await;
    assert_eq!(status, 200, "{init}");
    agent.did("ebook#open", json!({})).await;
    agent
        .did(
            "ebook#shelve",
            json!({"book": "book:dusk", "title": "Dusk", "author": "Ada", "subject": "Visas",
                   "shared": false}),
        )
        .await;

    // Ada's tab holds her root: it is her approver at the relay.
    let host = MemberApprover::new(on_relay.relay_host());

    // Refused, the agent's call fails with the refusal, and nothing ran.
    let (refused, seen) = Box::pin(answered_by_page(
        &mut agent,
        &host,
        &ada,
        ConfirmationAnswer::Deny,
    ))
    .await;
    assert!(
        refused["result"]["isError"] == true && refused.to_string().contains("did not allow"),
        "a refusal fails the call: {refused}"
    );
    let asked = seen.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "asked once: {asked:?}");
    // The sheet says who asks (the agent by its key's name, as History
    // names its changes), in which run, exactly what it would do as the app
    // declares it, on which record and where.
    let question = &asked[0];
    assert_eq!(question.agent_label, "Jane Doe’s agent");
    assert_eq!(question.run, RUN);
    assert_eq!(question.action_label, "Remove from library");
    assert_eq!(
        question.resource_label, "1 book",
        "the library by its name: named by the state it holds, one book on the shelf"
    );
    assert_eq!(
        question.arguments,
        [("Book".to_owned(), "book:dusk".to_owned())]
    );
    assert_eq!(question.workspace, "Applicant: Jane Doe");
    assert_eq!(
        (question.risk.as_str(), question.undoable),
        ("destructive", false)
    );

    // Allowed, the same call runs.
    let (allowed, seen) = Box::pin(answered_by_page(
        &mut agent,
        &host,
        &ada,
        ConfirmationAnswer::AllowOnce,
    ))
    .await;
    assert_eq!(allowed["result"]["isError"], false, "it ran: {allowed}");
    assert_eq!(seen.asked.lock().unwrap().len(), 1);
    host.close().await;
}
