//! The MCP front door against the relay, as tilo.sh runs them: the relay's
//! own person is its operator, the site's home workspace lets people in
//! through its door, and each person keeps one workspace of their own there.
//! A person's key works through the door's HTTP routes alone: it makes the
//! person's workspace, is answered there by the relay's app workers with no
//! tab of anyone's open, mints a key that reaches that one workspace and
//! nothing else, and revoking it ends both.
use super::door_on_relay::{approved_key, door_on_relay, door_on_relay_limiting, headers, json_of};
use super::hosted_documents::{application_pdf, content_in, json_in};
use super::*;
use base64::Engine as _;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_front_door::Door;
use tilo_user_identity::MAX_AGENT_GRANT_LIFETIME_NS;

pub(super) const ISSUER: &str = "https://mcp.tilo.test";

/// An MCP session at the door's `path` with `token`.
pub(super) struct Mcp<'a> {
    pub(super) door: &'a Arc<Door>,
    pub(super) path: String,
    pub(super) token: String,
    pub(super) session: Option<String>,
}

impl Mcp<'_> {
    /// Post `message`: the HTTP status and the reply.
    pub(super) async fn post(&mut self, message: Value) -> (u16, Value) {
        let bearer = format!("Bearer {}", self.token);
        let mut pairs = vec![
            ("authorization", bearer.as_str()),
            ("accept", "application/json"),
            ("content-type", "application/json"),
        ];
        if let Some(session) = &self.session {
            pairs.push((tilo_front_door::session::SESSION_HEADER, session.as_str()));
        }
        let reply = self
            .door
            .dispatch(
                &hyper::Method::POST,
                &self.path,
                "",
                &headers(&pairs),
                message.to_string().as_bytes(),
            )
            .await;
        if let Some(session) = reply
            .headers()
            .get(tilo_front_door::session::SESSION_HEADER)
        {
            self.session = Some(session.to_str().unwrap().to_owned());
        }
        json_of(reply).await
    }

    pub(super) async fn initialize(&mut self) -> (u16, Value) {
        self.post(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                         "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                    "clientInfo": {"name": "visa-backend", "version": "1"}}}))
            .await
    }

    /// `tilo_do` with `arguments`, its typed answer: what the SDKs read.
    pub(super) async fn tilo_do(&mut self, arguments: Value) -> Value {
        let (status, reply) = self
            .post(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": "tilo_do", "arguments": arguments,
                                    "_meta": {"so.tilo.mcp": {"structured": true}}}}))
            .await;
        assert_eq!(status, 200, "{reply}");
        reply
    }

    /// What `tilo_do` of `path` with `args` answered once it ran, asked
    /// again after each wait a busy host said, as the SDKs do; and how many
    /// times it was told to wait.
    async fn did_patiently(&mut self, path: &str, args: Value) -> (Value, u32) {
        let mut waits = 0;
        loop {
            let reply = self.tilo_do(json!({"path": path, "args": args})).await;
            let answer = &reply["result"]["structuredContent"];
            if answer["status"] == "ok" {
                return (answer["result"].clone(), waits);
            }
            assert_eq!(answer["error"]["code"], "rate_limited", "{reply}");
            let wait = answer["error"]["retry_after_ms"]
                .as_u64()
                .unwrap_or_else(|| panic!("a busy answer says when to ask again: {reply}"));
            assert!(
                (1..=1000).contains(&wait),
                "ten changes a second: the next within a second, {wait} ms"
            );
            waits = waits.saturating_add(1);
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
    }

    /// What `tilo_do` of `path` with `args` answered, which it ran.
    pub(super) async fn did(&mut self, path: &str, args: Value) -> Value {
        let reply = self.tilo_do(json!({"path": path, "args": args})).await;
        let answer = &reply["result"]["structuredContent"];
        assert_eq!(answer["status"], "ok", "{path} ran: {reply}");
        answer["result"].clone()
    }

    /// The bytes of `content` the session gets back with `tilo_get`.
    async fn get(&mut self, content: &str) -> Vec<u8> {
        let (status, reply) = self
            .post(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
                         "params": {"name": "tilo_get", "arguments": {"content": content}}}))
            .await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let piece = json_in(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default(),
        );
        base64::engine::general_purpose::STANDARD
            .decode(piece["bytes"].as_str().expect("the piece's bytes"))
            .expect("base64")
    }
}

/// `PUT /w/<workspace>/content` of `body` in `mcp`'s session: the status
/// and the reply.
pub(super) async fn upload(mcp: &Mcp<'_>, workspace: &str, body: &[u8]) -> (u16, Value) {
    let bearer = format!("Bearer {}", mcp.token);
    let mut pairs = vec![
        ("authorization", bearer.as_str()),
        ("content-type", "application/pdf"),
    ];
    if let Some(session) = &mcp.session {
        pairs.push((tilo_front_door::session::SESSION_HEADER, session.as_str()));
    }
    json_of(
        mcp.door
            .dispatch(
                &hyper::Method::PUT,
                &format!("/w/{workspace}/content"),
                "",
                &headers(&pairs),
                body,
            )
            .await,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one key's story through the door, from its approval to its revocation"
)]
async fn a_key_works_through_the_door_on_the_relay_and_ends_when_revoked() {
    let on_relay = door_on_relay(ISSUER).await;
    let door = Arc::clone(&on_relay.door);
    let lobby = on_relay.lobby.clone();
    // A person who never met the operator approves a key for their backend.
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Visa backend").await;

    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key.clone(),
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");

    // It makes the person's workspace on the relay.
    let created = account
        .tilo_do(json!({"path": "account/workspaces#create",
                        "args": {"name": "Applicant: Jane Doe"}}))
        .await;
    let result = &created["result"]["structuredContent"]["result"];
    let workspace = result["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a workspace is made: {created}"))
        .to_owned();
    // A person has as many workspaces of their own there as they ask for: a
    // second, named, is another one.
    let second = account
        .tilo_do(json!({"path": "account/workspaces#create", "args": {"name": "Another"}}))
        .await;
    assert_eq!(
        second["result"]["structuredContent"]["status"], "ok",
        "{second}"
    );
    let another = second["result"]["structuredContent"]["result"]["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("another workspace is made: {second}"))
        .to_owned();
    assert_ne!(another, workspace);
    let listed = account
        .tilo_do(json!({"path": "account/workspaces#list"}))
        .await;
    let rows = listed["result"]["structuredContent"]["result"]["workspaces"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    // The two of their own, each named as theirs, and the site's home
    // workspace their key was let into, as an editor.
    let own: Vec<(&str, &str)> = rows
        .iter()
        .filter(|row| row["role"] == "owner")
        .filter_map(|row| Some((row["id"].as_str()?, row["name"].as_str()?)))
        .collect();
    assert_eq!(own.len(), 2, "{listed}");
    assert!(
        own.iter()
            .any(|(id, name)| *id == workspace && *name == "Applicant: Jane Doe")
            && own
                .iter()
                .any(|(id, name)| *id == another && *name == "Another"),
        "{listed}"
    );
    assert!(
        rows.iter()
            .any(|row| row["id"] == on_relay.lobby.as_str() && row["role"] == "editor"),
        "the home workspace the key was let into: {listed}"
    );

    // The relay's app workers answer it there, with no tab of anyone's
    // open.
    let (status, listed) = account
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_ls", "arguments": {"workspace": workspace}}}))
        .await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["result"]["isError"], false, "{listed}");

    // It mints a key for that workspace alone.
    let minted = account
        .tilo_do(json!({"path": "account/keys#mint", "args": {"workspace": workspace}}))
        .await;
    let minted = &minted["result"]["structuredContent"]["result"];
    // A key that lasts until revoked mints keys that still expire.
    let most = tilo_clock::unix_now_ms() + MAX_AGENT_GRANT_LIFETIME_NS / 1_000_000;
    assert!(
        minted["expires_at_ms"]
            .as_u64()
            .is_some_and(|expiry| expiry <= most),
        "{minted}"
    );
    let minted_key = minted["token"]
        .as_str()
        .unwrap_or_else(|| panic!("a key is minted: {minted}"))
        .to_owned();
    // It joins that workspace and is answered there.
    let mut minted_session = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted_key.clone(),
        session: None,
    };
    let (status, init) = minted_session.initialize().await;
    assert_eq!(status, 200, "a minted key reaches its workspace: {init}");
    // Its door is that workspace's, so it names none.
    let (status, listed) = minted_session
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_ls", "arguments": {}}}))
        .await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["result"]["isError"], false, "{listed}");
    // The person's own documents, through the door alone with the key for
    // that workspace, run by the relay's app workers though the relay is not
    // theirs: a form goes in over HTTP, PDF Viewer fills it, Writer writes a
    // letter and exports it, and the letter comes back out.
    let (status, put) = upload(&minted_session, &workspace, &application_pdf()).await;
    assert_eq!(status, 201, "the form goes into the workspace: {put}");
    let form = put["content"].as_str().unwrap().to_owned();
    let work = &mut minted_session;
    work.did("pdf-viewer#open", json!({})).await;
    let imported = work
        .did(
            "pdf-viewer#import-pdf-content",
            json!({"content": form, "name": "application.pdf"}),
        )
        .await;
    assert!(
        imported.to_string().contains("application.pdf"),
        "{imported}"
    );
    let fields = work.did("pdf-viewer#read-form-fields", json!({})).await;
    let surname = fields["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|field| field["name"] == "surname")
        .unwrap_or_else(|| panic!("the form has a surname: {fields}"))
        .clone();
    let edit = json!({"path": surname["path"], "op": "set",
                      "value": {"kind": "text", "value": "Okafor"}});
    work.did(
        "pdf-viewer/application.pdf#field-edit",
        json!({"edit": edit}),
    )
    .await;
    let filled = work.did("pdf-viewer#read-form-fields", json!({})).await;
    assert!(
        filled.to_string().contains("Okafor"),
        "the form is filled: {filled}"
    );
    work.did("word-processor#open", json!({})).await;
    work.did(
        "word-processor#append-paragraph",
        json!({"text": "Dear Visa Officer,"}),
    )
    .await;
    let exported = work
        .tilo_do(json!({"path": "word-processor#export-content", "args": {"format": "docx"}}))
        .await;
    let letter = work.get(&content_in(&exported.to_string())).await;
    assert!(letter.starts_with(b"PK"), "the letter is a Word file");
    // History names the key's agent, acting for the person, for the edit.
    let history = work
        .did("word-processor#history", json!({"last": 50}))
        .await;
    let ada_hex = hex::encode(ada.verifying_key().to_bytes());
    let edits: Vec<&Value> = history["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|version| version.get("actor").is_some())
        .collect();
    assert!(
        !edits.is_empty()
            && edits.iter().all(|version| {
                version["actor"]["kind"] == "agent" && version["actor"]["for"] == ada_hex.as_str()
            }),
        "{history}"
    );
    // The key puts nothing into another of the person's workspaces.
    let (status, _) = upload(work, &another, &application_pdf()).await;
    assert_eq!(
        status, 403,
        "a workspace's key puts only into its workspace"
    );

    // It reaches no other workspace: not the site's home workspace.
    let mut lobby_session = Mcp {
        door: &door,
        path: format!("/w/{lobby}/mcp"),
        token: minted_key.clone(),
        session: None,
    };
    let (status, _) = lobby_session.initialize().await;
    assert_eq!(status, 403, "a minted key reaches only its workspace");

    // The person's key reads what their workspaces keep and ran: no VM ran
    // for any of it. The minted key reads no account.
    let usage = |id| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
               "params": {"name": "tilo_read", "arguments": {"path": "account/usage"},
                          "_meta": {"so.tilo.mcp": {"structured": true}}}})
    };
    let (status, read) = account.post(usage(6)).await;
    assert_eq!(status, 200, "{read}");
    let rows = read["result"]["structuredContent"]["fields"]["standing"]["usage"]["workspaces"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter()
            .any(|row| row["workspace"] == workspace.as_str()),
        "{read}"
    );
    assert!(
        rows.iter()
            .all(|row| row["vm_seconds"] == 0 && row["running"] == false),
        "{read}"
    );
    let mut minted_account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: minted_key.clone(),
        session: None,
    };
    let (status, init) = minted_account.initialize().await;
    assert_eq!(
        status, 200,
        "a minted key at the door's root is its workspace's: {init}"
    );
    let (status, read) = minted_account.post(usage(7)).await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(
        read["result"]["structuredContent"]["error"]["code"], "forbidden",
        "a minted key reads no account: {read}"
    );

    // Revoking the key that minted it ends both, at once.
    let keys = account.tilo_do(json!({"path": "account/keys#list"})).await;
    let own = keys["result"]["structuredContent"]["result"]["keys"][0]["grant"]
        .as_str()
        .unwrap_or_else(|| panic!("the key lists itself: {keys}"))
        .to_owned();
    let revoked = account
        .tilo_do(json!({"path": "account/keys#revoke", "args": {"grant": own}}))
        .await;
    assert_eq!(
        revoked["result"]["structuredContent"]["status"], "ok",
        "{revoked}"
    );
    let (status, _) = account
        .post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await;
    assert_eq!(status, 401, "the key's session ended");
    let (status, _) = minted_session
        .post(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}))
        .await;
    assert_eq!(status, 401, "the minted key's session ended with it");
    let mut minted_again = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted_key,
        session: None,
    };
    let (status, _) = minted_again.initialize().await;
    assert_eq!(status, 401, "the key it minted ended with it");
    assert_eq!(door.session_count(), 0);
}

/// A backend making workspaces faster than its person's budget refills is
/// answered `rate_limited`, with when to ask again, and nothing ran: asking
/// again then makes each one once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backend_past_its_budget_is_told_when_to_ask_again() {
    const BURST: u32 = 3;
    let on_relay = door_on_relay_limiting(
        ISSUER,
        Some(tilo_relay::member_limits::MemberLimitsConfig {
            owner_burst: BURST,
            owner_changes_per_window: 10,
            window_secs: 1,
            ..Default::default()
        }),
    )
    .await;
    let door = Arc::clone(&on_relay.door);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Customer backend").await;
    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");
    let wanted = BURST * 3;
    let mut refusals = 0_u32;
    let began = Instant::now();
    for customer in 0..wanted {
        let (_, waits) = account
            .did_patiently(
                "account/workspaces#create",
                json!({"name": format!("Customer {customer}")}),
            )
            .await;
        refusals = refusals.saturating_add(waits);
    }
    eprintln!(
        "{wanted} workspaces past a burst of {BURST} in {:?}, {refusals} times told to wait",
        began.elapsed()
    );
    assert!(refusals > 0, "past the burst, the budget paced it");
    let (listed, _) = account
        .did_patiently("account/workspaces#list", json!({}))
        .await;
    let owned = listed["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["role"] == "owner")
        .count();
    assert_eq!(
        owned,
        usize::try_from(wanted).unwrap(),
        "a refused create made nothing: {listed}"
    );
}

/// Workspaces a backend makes for its customers with one key, here.
const CUSTOMERS: usize = 500;

/// Of those, the ones it works in at once.
const AT_ONCE: usize = 20;

/// The `quantile` of `durations`.
fn quantile(durations: &[Duration], quantile: f64) -> Duration {
    let mut sorted = durations.to_vec();
    sorted.sort();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "an index into a short list"
    )]
    let index = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted[index]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_key_makes_and_works_in_hundreds_of_workspaces_at_once() {
    // The relay keeps each person to the budgets a public one does; a
    // person's server key spends the owner budget, which guests never reach.
    let on_relay = door_on_relay_limiting(
        ISSUER,
        Some(tilo_relay::member_limits::MemberLimitsConfig::default()),
    )
    .await;
    let door = Arc::clone(&on_relay.door);
    // A startup's backend, whose person is not the relay's operator, makes a
    // workspace per customer with its one key, as fast as it answers: none
    // is refused or paced (`did` takes nothing but `ok`).
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Customer backend").await;
    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key.clone(),
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");

    let mut made = Vec::with_capacity(CUSTOMERS);
    let mut creating = Vec::with_capacity(CUSTOMERS);
    for customer in 0..CUSTOMERS {
        let began = Instant::now();
        let created = account
            .did(
                "account/workspaces#create",
                json!({"name": format!("Customer {customer}")}),
            )
            .await;
        creating.push(began.elapsed());
        made.push(
            created["workspace"]["id"]
                .as_str()
                .unwrap_or_else(|| panic!("a workspace is made: {created}"))
                .to_owned(),
        );
    }
    let began = Instant::now();
    let listed = account.did("account/workspaces#list", json!({})).await;
    let listing = began.elapsed();
    let ids: std::collections::HashSet<&str> = listed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert!(
        made.iter().all(|id| ids.contains(id.as_str())),
        "every workspace is listed"
    );
    assert!(
        listed["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["role"] == "owner")
            .count()
            == CUSTOMERS,
        "each of theirs as its owner"
    );
    let (create_p50, create_p95) = (quantile(&creating, 0.5), quantile(&creating, 0.95));
    eprintln!(
        "{CUSTOMERS} workspaces: create p50 {create_p50:?}, p95 {create_p95:?}, max {:?}; list {listing:?}",
        quantile(&creating, 1.0)
    );
    assert!(
        create_p95 < Duration::from_millis(500),
        "making a workspace stays fast: p95 {create_p95:?}"
    );
    // The last hundred cost what the first hundred did (the very first warms
    // the relay up): nothing a create does, or that the workspaces made
    // before it do meanwhile, grows with how many the person has.
    let (first, last) = (
        quantile(&creating[1..=100], 0.95),
        quantile(&creating[CUSTOMERS - 100..], 0.95),
    );
    eprintln!("create p95: first hundred {first:?}, last hundred {last:?}");
    assert!(
        last <= first * 2 + Duration::from_millis(100),
        "the {CUSTOMERS}th workspace costs what the first ones did: p95 {first:?} at first, {last:?} at last"
    );
    assert!(
        listing < Duration::from_millis(500),
        "listing them stays fast: {listing:?}"
    );

    // It works in some of them at once, each a session of the same key, and
    // puts a file into one without naming a session while they are open:
    // none cuts another off.
    let began = Instant::now();
    let mut working = tokio::task::JoinSet::new();
    for workspace in made.iter().take(AT_ONCE).cloned() {
        let door = Arc::clone(&door);
        let token = key.clone();
        working.spawn(async move {
            let mut session = Mcp {
                door: &door,
                path: format!("/w/{workspace}/mcp"),
                token,
                session: None,
            };
            let began = Instant::now();
            let (status, init) = session.initialize().await;
            assert_eq!(status, 200, "{workspace}: {init}");
            let entered = began.elapsed();
            // Its first catalogue: the host joins the fresh workspace beside
            // every other one entered at once, and answers in time.
            let (status, listed) = session
                .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                             "params": {"name": "tilo_ls", "arguments": {}}}))
                .await;
            assert_eq!(status, 200, "{workspace}: {listed}");
            assert_eq!(listed["result"]["isError"], false, "{workspace}: {listed}");
            (workspace, session.session, entered, began.elapsed())
        });
    }
    let uploader = Mcp {
        door: &door,
        path: String::new(),
        token: key.clone(),
        session: None,
    };
    let (status, put) = upload(&uploader, &made[0], &application_pdf()).await;
    assert_eq!(
        status, 201,
        "a file goes in beside the open sessions: {put}"
    );
    let mut open = Vec::with_capacity(AT_ONCE);
    let (mut entering, mut answering) = (Vec::new(), Vec::new());
    while let Some(worked) = working.join_next().await {
        let (workspace, id, entered, answered) = worked.expect("a session worked");
        entering.push(entered);
        answering.push(answered);
        open.push((workspace, id));
    }
    let at_once = began.elapsed();
    eprintln!(
        "{AT_ONCE} sessions opened and answered at once in {at_once:?}: entered p50 {:?}, max {:?}; first catalogue answered p50 {:?}, max {:?}",
        quantile(&entering, 0.5),
        quantile(&entering, 1.0),
        quantile(&answering, 0.5),
        quantile(&answering, 1.0),
    );
    // About 3 s unloaded and up to 7 s on a busy builder; entering them one
    // after another took 13 s.
    assert!(
        at_once < Duration::from_secs(9),
        "working in {AT_ONCE} fresh workspaces at once does not wait for one after another: {at_once:?}"
    );

    // Every one of them, and the account's own, still answers.
    for (workspace, id) in open {
        let mut session = Mcp {
            door: &door,
            path: format!("/w/{workspace}/mcp"),
            token: key.clone(),
            session: id,
        };
        let (status, listed) = session
            .post(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                         "params": {"name": "tilo_ls", "arguments": {}}}))
            .await;
        assert_eq!(status, 200, "{workspace} still answers: {listed}");
        assert_eq!(listed["result"]["isError"], false, "{workspace}: {listed}");
    }
    let listed = account.did("account/workspaces#list", json!({})).await;
    assert!(
        made.iter().all(|made| listed["workspaces"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == made.as_str()))),
        "the account's own session still answers with every one of them"
    );
}

/// A minted key's agent goes by the name the key was minted with: History
/// names its changes "Jane Doe's agent", in the colour presence draws it in,
/// and its run by that name as the host knows it, never the key's grant id
/// nor anything the client says of itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_names_a_minted_keys_agent_by_the_name_it_was_minted_with() {
    let on_relay = door_on_relay(ISSUER).await;
    let door = Arc::clone(&on_relay.door);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Visa backend").await;
    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    assert_eq!(account.initialize().await.0, 200);
    let created = account
        .did(
            "account/workspaces#create",
            json!({"name": "Applicant: Jane Doe"}),
        )
        .await;
    let workspace = created["workspace"]["id"].as_str().unwrap().to_owned();
    let minted = account
        .did(
            "account/keys#mint",
            json!({"workspace": workspace, "name": "Jane Doe's agent"}),
        )
        .await;
    let mut agent = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    assert_eq!(agent.initialize().await.0, 200);
    agent.did("word-processor#open", json!({})).await;
    agent
        .did(
            "word-processor#append-paragraph",
            json!({"text": "Dear Visa Officer,"}),
        )
        .await;
    let history = agent
        .did("word-processor#history", json!({"last": 50}))
        .await;
    let by_agent: Vec<&Value> = history["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|version| version["actor"]["kind"] == "agent")
        .collect();
    let (red, green, blue) = collab_types::presence::agent_rgb("Jane Doe's agent");
    let colour = format!("#{red:02x}{green:02x}{blue:02x}");
    assert!(
        !by_agent.is_empty()
            && by_agent.iter().all(|version| {
                version["actor"]["name"] == "Jane Doe's agent"
                    && version["actor"]["colour"] == colour.as_str()
                    && version["run"] == "Jane Doe's agent"
            }),
        "{history}"
    );
}

/// Workspaces a backend works in at once while the relay is busy making more:
/// the shape of the burst a startup driving a workspace per customer sees,
/// large enough that a first answer queueing behind the others' joins cannot
/// be mistaken for a slow machine.
const WORKED_AT_ONCE: usize = 50;

/// Workspaces the same backend makes meanwhile, so the relay is busy with
/// other work than the burst's while the burst is answered.
const MADE_MEANWHILE: usize = 200;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspaces_joined_at_once_each_answer_their_first_catalogue() {
    let on_relay = door_on_relay_limiting(
        ISSUER,
        Some(tilo_relay::member_limits::MemberLimitsConfig::default()),
    )
    .await;
    let door = Arc::clone(&on_relay.door);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Customer backend").await;
    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key.clone(),
        session: None,
    };
    let (status, init) = account.initialize().await;
    assert_eq!(status, 200, "{init}");
    let mut made = Vec::with_capacity(WORKED_AT_ONCE);
    for customer in 0..WORKED_AT_ONCE {
        let created = account
            .did(
                "account/workspaces#create",
                json!({"name": format!("Customer {customer}")}),
            )
            .await;
        made.push(
            created["workspace"]["id"]
                .as_str()
                .unwrap_or_else(|| panic!("a workspace is made: {created}"))
                .to_owned(),
        );
    }

    // The same backend keeps making workspaces while it works in the ones it
    // has: the relay's app workers answer the burst beside that work, as they
    // answer a startup's hundreds.
    let busy = tokio::spawn({
        let door = Arc::clone(&door);
        let token = key.clone();
        async move {
            let mut making = Mcp {
                door: &door,
                path: "/mcp".into(),
                token,
                session: None,
            };
            assert_eq!(making.initialize().await.0, 200);
            for customer in WORKED_AT_ONCE..WORKED_AT_ONCE + MADE_MEANWHILE {
                making
                    .did(
                        "account/workspaces#create",
                        json!({"name": format!("Customer {customer}")}),
                    )
                    .await;
            }
        }
    });

    // Every one of them joined at once, each asking for its catalogue once:
    // the answer is the runtime's own bound — a call the host does not answer
    // within it fails, which none of these may.
    let mut working = tokio::task::JoinSet::new();
    for workspace in made {
        let door = Arc::clone(&door);
        let token = key.clone();
        working.spawn(async move {
            let mut session = Mcp {
                door: &door,
                path: format!("/w/{workspace}/mcp"),
                token,
                session: None,
            };
            let began = Instant::now();
            let (status, init) = session.initialize().await;
            assert_eq!(status, 200, "{workspace}: {init}");
            let entered = began.elapsed();
            let (status, listed) = session
                .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                             "params": {"name": "tilo_ls", "arguments": {}}}))
                .await;
            assert_eq!(status, 200, "{workspace}: {listed}");
            assert_eq!(
                listed["result"]["isError"], false,
                "{workspace} is answered within the wait a call allows: {listed}"
            );
            (entered, began.elapsed())
        });
    }
    let (mut entering, mut answering) = (Vec::new(), Vec::new());
    while let Some(worked) = working.join_next().await {
        let (entered, answered) = worked.expect("a session worked");
        entering.push(entered);
        answering.push(answered);
    }
    busy.await.expect("the backend made its workspaces");
    let (p50, p95, max) = (
        quantile(&answering, 0.5),
        quantile(&answering, 0.95),
        quantile(&answering, 1.0),
    );
    eprintln!(
        "{WORKED_AT_ONCE} workspaces joined and answered at once while {MADE_MEANWHILE} more were made: p50 {p50:?}, p95 {p95:?}, max {max:?}; entered p50 {:?}, max {:?}",
        quantile(&entering, 0.5),
        quantile(&entering, 1.0),
    );
    // The last of them costs what the middle one did: no first answer waits
    // for the joins before it, so the burst's tail is the machine's jitter,
    // not the queue's length.
    assert!(
        max <= p50 * 3 + Duration::from_secs(2),
        "{WORKED_AT_ONCE} answered at once: p50 {p50:?} but the slowest {max:?}, which is a queue"
    );
}
