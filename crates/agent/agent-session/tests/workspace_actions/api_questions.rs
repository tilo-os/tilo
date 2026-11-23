//! What an agent's call that asks first becomes, and how the person it waits for answers it.
//!
//! The call is kept as its own `tilo.invocations` record in status asked, listed for the person
//! by `tilo.asking`'s `waiting#list` at a device of theirs and answered there with the record's
//! `resolve`: approve runs the call the server kept, once, and decline refuses it. The answer
//! lands on the call's own invocation, which the agent follows.
use super::api::{ISSUER, Mcp};
use super::api_on_server::{OnServer, api_on_server, approved_key};
use super::*;
use serde_json::{Value, json};
use tilo_actions::InvocationId;

/// Removing a book from the library: Undo cannot give the reader back the
/// book's file, so the action asks first.
const REMOVE: &str = "ebook/1 book#remove-from-library";

/// The agent's call that asks first, and the person it waits for: the MCP session that made it,
/// the workspace it acts in, and the person's own page there.
pub(super) struct WithABook<'a> {
    pub(super) agent: Mcp<'a>,
    pub(super) page: Browser,
    pub(super) workspace: String,
}

/// Ada's workspace for a customer, with one book on its shelf, the agent her backend minted a
/// key for there, and Ada at a page of her own in it.
pub(super) async fn agent_with_a_book<'a>(
    on_server: &'a OnServer,
    ada: &SigningKey,
) -> WithABook<'a> {
    let page = enrol(ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(on_server, ada, &page, "Visa backend").await;
    let mut account = Mcp {
        api: &on_server.api,
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
        api: &on_server.api,
        path: format!("/w/{workspace}/mcp"),
        token,
        session: None,
    };
    let (status, init) = agent.initialize().await;
    assert_eq!(status, 200, "{init}");
    agent.did("ebook#open", json!({})).await;
    agent
        .did("ebook#shelve", shelved("dusk", "Dusk", "Visas"))
        .await;
    let page = Browser::open_at(&on_server.hosted.server, &page, &on_server.host, &workspace).await;
    WithABook {
        agent,
        page,
        workspace,
    }
}

/// `tilo_do` of [`REMOVE`] as the agent, which asks its person first: the invocation it waits in,
/// and the reply that says so.
async fn asking(agent: &mut Mcp<'_>) -> (InvocationId, Value) {
    let (status, reply) = agent
        .post(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                   "params": {"name": "tilo_do",
                              "arguments": {"path": REMOVE, "args": {"book": "book:dusk"}},
                              "_meta": {"so.tilo.mcp": {"structured": true}}}}))
        .await;
    assert_eq!(status, 200, "{reply}");
    let asked = &reply["result"]["structuredContent"];
    assert_eq!(
        asked["status"], "asked",
        "the call waits for its person: {reply}"
    );
    let invocation = asked["invocation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the record it waits in: {reply}"))
        .parse()
        .expect("an invocation id");
    (invocation, reply)
}

/// `invocation`'s answer once it stops waiting: how it ended, as the agent reads it.
pub(super) async fn followed(agent: &mut Mcp<'_>, invocation: InvocationId) -> Value {
    let answer = |reply: &Value| -> Option<Value> {
        let status = reply["result"]["structuredContent"]["status"].as_str()?;
        (status != "asked").then(|| reply["result"]["structuredContent"].clone())
    };
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    loop {
        let (status, reply) = agent
            .post(json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call",
                         "params": {"name": "tilo_status",
                                    "arguments": {"invocation_id": invocation.0.to_string()},
                                    "_meta": {"so.tilo.mcp": {"structured": true}}}}))
            .await;
        assert_eq!(status, 200, "{reply}");
        if let Some(answer) = answer(&reply) {
            return answer;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the invocation never stopped waiting: {reply}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// What shelving the book `book` under `title` takes, filed under `about`.
pub(super) fn shelved(book: &str, title: &str, about: &str) -> serde_json::Value {
    json!({
        "book": format!("book:{book}"),
        "title": title,
        "author": "Ada",
        "subject": about,
        "shared": false,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_call_waits_for_its_person_and_their_answer_runs_it_once() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let mut with = agent_with_a_book(&on_server, &ada).await;
    let (invocation, _) = asking(&mut with.agent).await;

    // The person's own device lists it among the questions that wait for them: who asks, what
    // would be done, on which record and in which workspace, and that Undo cannot take it back.
    let asked = with.page.asked_about(invocation).await;
    let tilo_actions::remote::asked::AskedBy::Agent(who) = &asked.facts.by else {
        panic!("an agent asks: {asked:?}");
    };
    assert_eq!(who.name, "Jane Doe’s agent", "{asked:?}");
    assert_eq!(asked.workspace_name, "Applicant: Jane Doe", "{asked:?}");
    assert_eq!(
        asked.record.workspace, with.workspace,
        "the asked record is kept in the workspace its call acts in: {asked:?}"
    );
    assert_eq!(asked.name, "Remove from library", "{asked:?}");
    assert_eq!(
        asked.facts.on.as_deref(),
        Some("1 book"),
        "the library by its name: named by the state it holds, one book on the shelf: {asked:?}"
    );
    assert!(
        asked.facts.destructive,
        "Undo cannot give the reader back the book's file: {asked:?}"
    );
    assert_eq!(
        asked.facts.facts,
        [tilo_actions::remote::asked::Fact::new("Book", "book:dusk")]
    );

    // Declined, the call is refused and nothing ran.
    with.page
        .resolve(&asked, "decline", None)
        .await
        .expect("the person declines");
    let refused = followed(&mut with.agent, invocation).await;
    assert_eq!(refused["status"], "failed", "{refused}");
    assert_eq!(refused["error"]["code"], "cancelled", "{refused}");
    assert_eq!(refused["error"]["message"], "declined", "{refused}");
    assert!(
        with.page.asks().await.is_empty(),
        "answered, it waits for nobody"
    );

    // Allowed, the same call runs.
    let (invocation, _) = asking(&mut with.agent).await;
    let asked = with.page.asked_about(invocation).await;
    with.page
        .resolve(&asked, "approve", None)
        .await
        .expect("the person approves");
    let ran = followed(&mut with.agent, invocation).await;
    assert_eq!(ran["status"], "ok", "it ran: {ran}");
    with.page.peer.close();
}
