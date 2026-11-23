//! The API's `/v1` routes against a workspace's server, as tilo.sh runs them: a
//! person's key makes a workspace (a retry of the same request makes no
//! second one), puts a form in, runs actions on it, follows its runs and
//! reads the workspace's events as they happen, is refused a webhook
//! (`account/webhooks#create`) to the API's own machine, mints a key for that workspace alone, and
//! deletes the workspace. Every route is one of the calls an agent makes
//! over MCP, so this is that same path through HTTP and JSON.

use super::api::{ISSUER, Mcp};
use super::api_on_server::{OnServer, api_on_server, approved_key, headers, json_of};
use super::hosted_documents::application_pdf;
use super::*;
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_actions::remote::{person_models, person_vault};
use tilo_api::Server;

/// A key's calls of the API.
struct V1<'a> {
    api: &'a Arc<Server>,
    token: String,
}

impl V1<'_> {
    async fn send(
        &self,
        method: hyper::Method,
        path: &str,
        body: &[u8],
        extra: &[(&'static str, &str)],
    ) -> tilo_api::Reply {
        let bearer = format!("Bearer {}", self.token);
        let mut pairs = vec![
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ];
        pairs.extend_from_slice(extra);
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        self.api
            .dispatch(&method, path, query, &headers(&pairs), body)
            .await
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        json_of(self.send(hyper::Method::GET, path, b"", &[]).await).await
    }

    async fn post(&self, path: &str, body: &Value, extra: &[(&'static str, &str)]) -> (u16, Value) {
        let reply = self
            .send(
                hyper::Method::POST,
                path,
                body.to_string().as_bytes(),
                extra,
            )
            .await;
        json_of(reply).await
    }

    /// Run the account's `action` (`workspaces#create`) as any action is run: the status, and
    /// what it answered, or why it was refused.
    async fn account(
        &self,
        action: &str,
        body: &Value,
        extra: &[(&'static str, &str)],
    ) -> (u16, Value) {
        let (status, answer) = self.post(&account_action(action), body, extra).await;
        (
            status,
            if status == 200 {
                answer["result"].clone()
            } else {
                answer
            },
        )
    }
}

/// Where the account's `action` (`keys#revoke`) is run.
fn account_action(action: &str) -> String {
    let (resource, action) = action.split_once('#').expect("resource#action");
    format!("/v1/resources/account/{resource}/actions/{action}")
}

/// A webhook's receiver: a server on this machine that takes every delivery, and what it
/// was posted.
struct HookReceiver {
    served: test_http::Responder,
    posted: Arc<(
        std::sync::Mutex<Vec<test_http::Request>>,
        tokio::sync::Notify,
    )>,
}

impl HookReceiver {
    fn start() -> Self {
        let posted = Arc::new((
            std::sync::Mutex::new(Vec::new()),
            tokio::sync::Notify::new(),
        ));
        let took = Arc::clone(&posted);
        let served = test_http::Responder::handle(move |request| {
            took.0.lock().unwrap().push(request.clone());
            took.1.notify_one();
            Some(test_http::Reply::status(200))
        });
        Self { served, posted }
    }

    /// Where it listens on this machine.
    fn at(&self) -> std::net::SocketAddr {
        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, self.served.port()))
    }

    /// The URL a webhook to it names.
    fn url(&self) -> String {
        format!("{}/hook", self.served.url())
    }

    /// The next delivery posted to it.
    async fn next(&self) -> test_http::Request {
        let next = async {
            loop {
                let first = {
                    let mut posted = self.posted.0.lock().unwrap();
                    (!posted.is_empty()).then(|| posted.remove(0))
                };
                if let Some(first) = first {
                    return first;
                }
                self.posted.1.notified().await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), next)
            .await
            .expect("the webhook's receiver is posted an event within 30 s")
    }
}

/// `posted` is one delivery of the webhook `hook`, as its maker was answered it: a POST that
/// names the webhook and its delivery, signed with the webhook's secret, whose body is the
/// event of an invocation.
fn is_a_signed_delivery_of(posted: &test_http::Request, hook: &Value) {
    assert_eq!(
        (posted.method.as_str(), posted.target.as_str()),
        ("POST", "/hook")
    );
    assert_eq!(posted.header("tilo-webhook-id"), hook["id"].as_str());
    assert!(
        posted
            .header("tilo-delivery")
            .is_some_and(|delivery| !delivery.is_empty()),
        "a delivery is named"
    );
    let said = posted
        .header("tilo-signature")
        .expect("a delivery is signed");
    let time: u64 = said
        .strip_prefix("t=")
        .and_then(|rest| rest.split(',').next())
        .and_then(|time| time.parse().ok())
        .expect("a signature says its time");
    let secret = hook["secret"]
        .as_str()
        .expect("a webhook says its secret when it is made");
    assert!(
        said == tilo_api::v1::webhooks::signature(secret, time, &posted.body),
        "a delivery is signed with its webhook's secret"
    );
    let event: Value =
        serde_json::from_slice(&posted.body).expect("a delivery's body is its event");
    assert_eq!(event["type"], json!("invocation"), "{event}");
}

/// A hosted API posts a workspace's event to a webhook, signed. The receiver is a server this
/// test starts on this machine and hands the API to stand in for the public internet, which a
/// test build alone can do. The one beside it was handed to nobody: it is refused as every
/// address of the API's own machine is, and nothing connects to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_apis_webhook_reaches_its_receiver_signed() {
    // A story of its own, on the heap as the long one's is: the long one's frame takes no
    // more.
    Box::pin(a_webhook_of_a_hosted_api_is_delivered()).await;
}

async fn a_webhook_of_a_hosted_api_is_delivered() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let backend = V1 {
        api: &api,
        token: approved_key(&on_server, &ada, &page, "Visa backend").await,
    };
    let (status, made) = backend
        .account(
            "workspaces#create",
            &json!({"name": "Applicant: Jane Doe"}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "{made}");
    let workspace = made["workspace"]["id"].as_str().unwrap();
    let base = format!("/v1/workspaces/{workspace}");

    let receiver = HookReceiver::start();
    let beside = test_http::Responder::silent();
    api.webhooks_stand_in(receiver.at());
    let (status, refused) = backend
        .account(
            "webhooks#create",
            &json!({"workspace": workspace, "url": format!("{}/hook", beside.url()), "events": ["invocation"]}),
            &[],
        )
        .await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (400, &json!("invalid_arguments")),
        "a hosted API kept a webhook to a receiver nobody handed it: {refused}"
    );
    let (status, hook) = backend
        .account(
            "webhooks#create",
            &json!({"workspace": workspace, "url": receiver.url(), "events": ["invocation"]}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "the webhook to the receiver is kept: {hook}");

    // An app is opened and takes a file, as the long story begins.
    let named = [
        ("tilo-invocation", "fill-jane"),
        ("tilo-invocation-title", "Fill Jane's application"),
    ];
    let (status, opened) = backend
        .post(
            &format!("{base}/resources/pdf-viewer/actions/open"),
            &json!({}),
            &named,
        )
        .await;
    assert_eq!(status, 200, "{opened}");
    let pdf = application_pdf();
    let hash = blake3::hash(&pdf).to_hex().to_string();
    let uploaded = backend
        .send(
            hyper::Method::PUT,
            &format!("{base}/contents/{hash}"),
            &pdf,
            &[("content-type", "application/pdf")],
        )
        .await;
    let (status, put) = json_of(uploaded).await;
    assert_eq!(status, 201, "{put}");
    let (status, imported) = backend
        .post(
            &format!("{base}/resources/pdf-viewer/actions/import-pdf-content"),
            &json!({"content": put["content"], "name": "application.pdf"}),
            &named,
        )
        .await;
    assert_eq!(
        (status, &imported["status"]),
        (200, &json!("ok")),
        "{imported}"
    );

    // An event of it reached the webhook: posted to the receiver, naming its webhook and its
    // delivery, signed with the secret its maker was answered once. The receiver beside it
    // was sent nothing, and the webhook is listed as one that is delivered.
    let posted = receiver.next().await;
    is_a_signed_delivery_of(&posted, &hook);
    beside.assert_untouched();
    let (status, kept) = backend
        .account("webhooks#list", &json!({"workspace": workspace}), &[])
        .await;
    assert_eq!(status, 200, "{kept}");
    let rows = kept["webhooks"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{kept}");
    assert_eq!(rows[0]["url"], json!(receiver.url()), "{kept}");
    assert!(rows[0].get("undelivered").is_none(), "{kept}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_works_through_the_api_on_a_server_host() {
    // The story builds a server, its API and a workspace's sessions; its future goes on the
    // heap rather than into the frames of the test thread that polls it.
    Box::pin(a_backend_key_works_through_the_api_on_a_server_host()).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "one backend's story through the API, from its first workspace to deleting it"
)]
async fn a_backend_key_works_through_the_api_on_a_server_host() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let backend = V1 {
        api: &api,
        token: approved_key(&on_server, &ada, &page, "Visa backend").await,
    };

    // The API says its version, and what the key reaches.
    let (status, info) = backend.get("/v1").await;
    assert_eq!((status, &info["api_version"]), (200, &json!("1")), "{info}");
    assert_eq!(info["key"]["scope"], "all", "{info}");

    // A workspace, made once however often the same request is retried.
    let new = json!({"name": "Applicant: Jane Doe"});
    let retry = [("idempotency-key", "make-jane")];
    let (status, made) = backend.account("workspaces#create", &new, &retry).await;
    assert_eq!(status, 200, "{made}");
    let workspace = made["workspace"]["id"].as_str().unwrap().to_owned();
    let replayed = backend
        .send(
            hyper::Method::POST,
            &account_action("workspaces#create"),
            new.to_string().as_bytes(),
            &retry,
        )
        .await;
    assert_eq!(replayed.headers()["idempotent-replayed"], "true");
    let (_, again) = json_of(replayed).await;
    assert_eq!(
        again["result"]["workspace"]["id"], made["workspace"]["id"],
        "a retry answers the first workspace"
    );
    let (status, conflict) = backend
        .account(
            "workspaces#create",
            &json!({"name": "Someone else"}),
            &retry,
        )
        .await;
    assert_eq!(
        (status, &conflict["error"]["code"]),
        (409, &json!("idempotency_conflict"))
    );
    let (status, listed) = backend.account("workspaces#list", &json!({}), &[]).await;
    assert_eq!(status, 200, "{listed}");
    let named = listed["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["name"] == "Applicant: Jane Doe")
        .count();
    assert_eq!(named, 1, "one workspace for the retried request: {listed}");

    // A file goes in, and an action takes it.
    let base = format!("/v1/workspaces/{workspace}");
    let pdf = application_pdf();
    let hash = blake3::hash(&pdf).to_hex().to_string();
    let uploaded = backend
        .send(
            hyper::Method::PUT,
            &format!("{base}/contents/{hash}"),
            &pdf,
            &[("content-type", "application/pdf")],
        )
        .await;
    let (status, put) = json_of(uploaded).await;
    assert_eq!(status, 201, "{put}");
    let form = put["content"].as_str().unwrap().to_owned();

    // The workspace's events, read as they happen while the key works.
    let stream = backend
        .send(hyper::Method::GET, &format!("{base}/events"), b"", &[])
        .await;
    assert_eq!(stream.headers()["content-type"], "text/event-stream");
    let mut stream = stream.into_body();
    // A hosted API posts webhooks to the public internet only. A URL on its own machine or
    // network is refused, over plain http and over https, whatever the request says of where
    // it comes from, and nothing of it is kept. (Inside a desktop the API posts to that
    // computer's own loopback: signed, and tried again the same, in tilo-api's own tests.)
    let claims = [
        ("x-forwarded-for", "127.0.0.1"),
        ("forwarded", "for=127.0.0.1;proto=http"),
    ];
    for own in [
        "http://127.0.0.1:8080/hook",
        "http://localhost:8080/hook",
        "https://127.0.0.1:8443/hook",
        "https://169.254.169.254/hook",
        "https://10.0.0.7/hook",
    ] {
        let (status, refused) = backend
            .account(
                "webhooks#create",
                &json!({"workspace": workspace, "url": own, "events": ["invocation"]}),
                &claims,
            )
            .await;
        assert_eq!(
            (status, &refused["error"]["code"]),
            (400, &json!("invalid_arguments")),
            "a hosted API kept a webhook to {own}: {refused}"
        );
    }
    let (status, kept) = backend
        .account("webhooks#list", &json!({"workspace": workspace}), &[])
        .await;
    assert_eq!((status, &kept["webhooks"]), (200, &json!([])), "{kept}");

    let run_headers = [
        ("tilo-invocation", "fill-jane"),
        ("tilo-invocation-title", "Fill Jane's application"),
    ];
    let (status, opened) = backend
        .post(
            &format!("{base}/resources/pdf-viewer/actions/open"),
            &json!({}),
            &run_headers,
        )
        .await;
    assert_eq!(status, 200, "{opened}");
    let args = json!({"content": form, "name": "application.pdf"});
    let (status, imported) = backend
        .post(
            &format!("{base}/resources/pdf-viewer/actions/import-pdf-content"),
            &args,
            &[
                ("idempotency-key", "import-1"),
                run_headers[0],
                run_headers[1],
            ],
        )
        .await;
    assert_eq!(status, 200, "{imported}");
    assert_eq!(imported["status"], "ok", "{imported}");
    let run = imported["invocation"].as_str().unwrap().to_owned();
    // Retried, it answers the same run and imports nothing twice.
    let (_, retried) = backend
        .post(
            &format!("{base}/resources/pdf-viewer/actions/import-pdf-content"),
            &args,
            &[
                ("idempotency-key", "import-1"),
                run_headers[0],
                run_headers[1],
            ],
        )
        .await;
    assert_eq!(retried["invocation"], json!(run), "{retried}");
    let (status, fields) = backend
        .post(
            &format!("{base}/resources/pdf-viewer/application.pdf/actions/read-form-fields"),
            &json!({}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "{fields}");
    assert!(fields["result"].to_string().contains("surname"), "{fields}");

    // The run, followed by its id.
    let (status, followed) = backend.get(&format!("{base}/invocations/{run}")).await;
    assert_eq!(
        (status, &followed["status"]),
        (200, &json!("ok")),
        "{followed}"
    );

    // What happened came down the stream as it happened.
    let mut seen = String::new();
    while !seen.contains("event: invocation") {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(30), stream.frame())
            .await
            .expect("the stream says what happened within 30 s")
            .expect("the stream stays open")
            .unwrap();
        if let Ok(data) = frame.into_data() {
            seen.push_str(&String::from_utf8_lossy(&data));
        }
    }
    assert!(
        seen.contains("\"invocation\":"),
        "an invocation's event names it: {seen}"
    );

    // A refusal has one shape.
    let (status, refused) = backend
        .post(
            &format!("{base}/resources/pdf-viewer/actions/no-such-action"),
            &json!({}),
            &[],
        )
        .await;
    assert!((400..500).contains(&status), "{status} {refused}");
    assert!(
        refused["error"]["code"].is_string() && refused["error"]["message"].is_string(),
        "{refused}"
    );

    // A key for that workspace alone reaches it and nothing else.
    let (status, minted) = backend
        .account(
            "keys#mint",
            &json!({"workspace": workspace, "name": "Jane's agent"}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "{minted}");
    let agent = V1 {
        api: &api,
        token: minted["token"].as_str().unwrap().to_owned(),
    };
    let (status, listing) = agent.get(&format!("{base}/resources")).await;
    assert_eq!(status, 200, "{listing}");
    // A listing is its rows: no words beside them.
    assert!(listing.get("text").is_none(), "{listing}");
    let (status, others) = agent
        .get(&format!("/v1/workspaces/{}/resources", on_server.lobby))
        .await;
    assert_eq!(
        (status, &others["error"]["code"]),
        (403, &json!("beyond_scope")),
        "{others}"
    );
    let (status, minting) = agent
        .account("keys#mint", &json!({"workspace": workspace}), &[])
        .await;
    assert!(status >= 400, "a minted key mints none: {minting}");

    // The workspace records the key it admitted, so an API server that never
    // saw the approval lists it from the workspace itself.
    let second = Server::new(
        &tilo_api::Config {
            issuer: ISSUER.into(),
            web_entry: on_server.web_entry.clone(),
            hosts: vec![on_server.hosted.web.host_info().clone()],
        },
        tilo_api::store::Store::in_memory(),
    )
    .unwrap();
    let elsewhere = V1 {
        api: &second,
        token: minted["token"].as_str().unwrap().to_owned(),
    };
    let (status, listed) = elsewhere.account("keys#list", &json!({}), &[]).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(
        listed["keys"][0]["grant"], minted["grant"],
        "the workspace lists the key it admitted: {listed}"
    );

    // Revoking it writes the workspace's own record, so the API server that
    // never saw the approval refuses it too.
    let (status, revoked) = json_of(
        backend
            .send(
                hyper::Method::POST,
                &account_action("keys#revoke"),
                json!({"grant": minted["grant"]}).to_string().as_bytes(),
                &[("idempotency-key", "revoke-jane")],
            )
            .await,
    )
    .await;
    assert_eq!(status, 200, "{revoked}");
    let (status, refused) = elsewhere.account("keys#list", &json!({}), &[]).await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (401, &json!("key_revoked")),
        "{refused}"
    );

    // Deleting the workspace revokes the key that reached only it.
    let (status, minted) = backend
        .account(
            "keys#mint",
            &json!({"workspace": workspace, "name": "Jane's second agent"}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "{minted}");
    let agent = V1 {
        api: &api,
        token: minted["token"].as_str().unwrap().to_owned(),
    };
    let (status, deleted) = backend
        .account(
            "workspaces#delete",
            &json!({ "workspace": workspace }),
            &[("idempotency-key", "drop-jane")],
        )
        .await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(
        deleted["revoked_keys"],
        json!([minted["grant"]]),
        "{deleted}"
    );
    let (status, gone) = agent.get(&format!("{base}/resources")).await;
    assert_eq!(
        (status, &gone["error"]["code"]),
        (401, &json!("key_revoked")),
        "{gone}"
    );
}

/// Every key a person has is listed and revoked from any key of theirs that reaches all their
/// workspaces, whichever API server made it: a computer's own login, answered by the API in its
/// own process, lists a script's key that the site's API server approved (named, reaching all
/// workspaces, until when), before the script used it, and revokes it, after which the workspace
/// refuses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_key_of_a_person_is_listed_and_revoked_from_any_api_server() {
    Box::pin(a_persons_keys_are_listed_and_revoked_from_any_api_server()).await;
}

async fn a_persons_keys_are_listed_and_revoked_from_any_api_server() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    // A script's key, approved through the site's API server as `tilo keys export` asks for one.
    let script = approved_key(&on_server, &ada, &page, "Linear import").await;
    // The script works in a workspace of Ada's, its session with the server open.
    let working = V1 {
        api: &on_server.api,
        token: script.clone(),
    };
    let (status, made) = working
        .account(
            "workspaces#create",
            &json!({"name": "Imported issues"}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "{made}");
    let base = format!(
        "/v1/workspaces/{}",
        made["workspace"]["id"].as_str().unwrap()
    );
    let (listing, reading, writing) = (
        format!("{base}/resources"),
        format!("{base}/resources/tasks/tasks.issues"),
        format!("{base}/resources/tasks/tasks.issues/actions/create"),
    );
    let (status, wrote) = working
        .post(&writing, &json!({"title": "First issue"}), &[])
        .await;
    assert_eq!(status, 200, "{wrote}");
    let (status, listed) = working.get(&listing).await;
    assert_eq!(status, 200, "{listed}");
    let (status, read) = working.get(&reading).await;
    assert_eq!(status, 200, "{read}");
    let open = |api: &Server| {
        api.open_sessions()
            .iter()
            .any(|session| session.starts_with("Linear import"))
    };
    assert!(open(&on_server.api), "{:?}", on_server.api.open_sessions());
    // The computer's own login, answered by the API in its own process from its key file, as
    // `tilo keys ls` is: an API server that never saw the script's key approved.
    let laptop = enrol(&ada, "Claude Code on laptop", DeviceCapabilities::member());
    let file = tilo_api::direct::KeyFile::new(
        &on_server.server,
        Some(&on_server.web_entry),
        "Claude Code on laptop",
        &laptop.key,
        &laptop.cert,
    )
    .reaching_at(on_server.hosted.web.host_info().clone());
    let computer = tilo_api::direct::Direct::new(&file).unwrap();
    let json = [("content-type".to_owned(), "application/json".to_owned())];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let listed_script = loop {
        let (status, listed) = json_of(
            computer
                .request("POST", &account_action("keys#list"), &json, b"{}")
                .await,
        )
        .await;
        assert_eq!(status, 200, "{listed}");
        if let Some(row) = listed["result"]["keys"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| {
                row["name"]
                    .as_str()
                    .is_some_and(|name| name == "Linear import")
            })
        {
            break row.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the script's key is not among the person's keys: {listed}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    assert_eq!(listed_script["scope"], "all", "{listed_script}");
    assert!(
        listed_script.get("expires_at_ms").is_some(),
        "a key says until when it lasts (null: until revoked): {listed_script}"
    );
    let grant = listed_script["grant"].as_str().unwrap().to_owned();

    let revoking = [
        json[0].clone(),
        (
            "idempotency-key".to_owned(),
            "revoke-linear-import".to_owned(),
        ),
    ];
    let (status, revoked) = json_of(
        computer
            .request(
                "POST",
                &account_action("keys#revoke"),
                &revoking,
                json!({ "grant": grant }).to_string().as_bytes(),
            )
            .await,
    )
    .await;
    assert_eq!(status, 200, "{revoked}");
    let (_, listed) = json_of(
        computer
            .request("POST", &account_action("keys#list"), &json, b"{}")
            .await,
    )
    .await;
    assert!(
        listed["result"]["keys"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|row| row["grant"] != grant.as_str()),
        "a revoked key is listed no more: {listed}"
    );
    // Its open session ends, and it is refused on its next list, read and write alike.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while open(&on_server.api) {
        assert!(
            std::time::Instant::now() < deadline,
            "a revoked key's session stays open: {:?}",
            on_server.api.open_sessions()
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (list, read, write) = (
        working.get(&listing).await,
        working.get(&reading).await,
        working
            .post(&writing, &json!({"title": "After revocation"}), &[])
            .await,
    );
    for (call, (status, refused)) in [("list", list), ("read", read), ("write", write)] {
        assert_eq!(
            (status, &refused["error"]["code"]),
            (401, &json!("key_revoked")),
            "a revoked key's {call}: {refused}"
        );
    }
    assert!(
        !open(&on_server.api),
        "a refused call opens no session: {:?}",
        on_server.api.open_sessions()
    );
    // The workspace keeps the revocation: the script's key enters it no more, from wherever it
    // comes (here an API server it reaches the workspace through anew, as a script started again
    // does).
    let anew = Server::new(
        &tilo_api::Config {
            issuer: ISSUER.into(),
            web_entry: on_server.web_entry.clone(),
            hosts: vec![on_server.hosted.web.host_info().clone()],
        },
        tilo_api::store::Store::in_memory(),
    )
    .unwrap();
    let script = V1 {
        api: &anew,
        token: working.token.clone(),
    };
    let (status, refused) = script.account("keys#list", &json!({}), &[]).await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (401, &json!("key_revoked")),
        "{refused}"
    );
}

/// A program holding a key of the person's own (a device of theirs, not a
/// grant an API server keeps) reaches the server directly: the API served in
/// process, the same routes and answers, with no API server between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_its_program_holds_reaches_the_host_directly() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    // Ada comes to the server once, as every person does.
    approved_key(&on_server, &ada, &page, "Visa backend").await;
    let laptop = enrol(&ada, "Ada's script", DeviceCapabilities::member());
    let file = tilo_api::direct::KeyFile::new(
        &on_server.server,
        Some(&on_server.web_entry),
        "Ada's script",
        &laptop.key,
        &laptop.cert,
    )
    .reaching_at(on_server.hosted.web.host_info().clone());
    let direct = tilo_api::direct::Direct::new(&file).unwrap();
    let json = [("content-type".to_owned(), "application/json".to_owned())];

    let (status, info) = json_of(direct.request("GET", "/v1", &[], b"").await).await;
    assert_eq!(
        (status, &info["key"]["scope"]),
        (200, &json!("all")),
        "{info}"
    );
    let new = json!({"name": "Made from a script"}).to_string();
    let retried = [
        json[0].clone(),
        ("idempotency-key".to_owned(), "script-1".to_owned()),
    ];
    let (status, made) = json_of(
        direct
            .request(
                "POST",
                &account_action("workspaces#create"),
                &retried,
                new.as_bytes(),
            )
            .await,
    )
    .await;
    assert_eq!(status, 200, "{made}");
    let made = &made["result"]["workspace"];
    let (_, again) = json_of(
        direct
            .request(
                "POST",
                &account_action("workspaces#create"),
                &retried,
                new.as_bytes(),
            )
            .await,
    )
    .await;
    assert_eq!(
        again["result"]["workspace"]["id"], made["id"],
        "a retry makes no second workspace"
    );
    let workspace = made["id"].as_str().unwrap();
    let (status, listed) = json_of(
        direct
            .request("POST", &account_action("workspaces#list"), &json, b"{}")
            .await,
    )
    .await;
    assert_eq!(status, 200, "{listed}");
    assert!(listed.to_string().contains(workspace), "{listed}");
    let (status, opened) = json_of(
        direct
            .request(
                "POST",
                &format!("/v1/workspaces/{workspace}/resources/tasks/actions/open"),
                &json,
                b"{}",
            )
            .await,
    )
    .await;
    assert_eq!(
        status, 200,
        "the server's app workers answer the key: {opened}"
    );
    let (status, apps) = json_of(
        direct
            .request(
                "GET",
                &format!("/v1/workspaces/{workspace}/resources"),
                &[],
                b"",
            )
            .await,
    )
    .await;
    assert_eq!(status, 200, "{apps}");
    assert!(
        apps["rows"].as_array().is_some_and(|rows| !rows.is_empty()),
        "{apps}"
    );
}

/// One listing, one text: `GET …/resources` answers exactly what the
/// `tilo_ls` tool answers an agent in the same workspace. `tilo ls` prints
/// that text as it comes, so a person and an agent read the same listing and
/// neither re-renders the other's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspaces_listing_reads_the_same_through_mcp_and_the_api() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &ada, &page, "Notes backend").await;
    let mut account = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: key.clone(),
        session: None,
    };
    assert_eq!(account.initialize().await.0, 200);
    let made = account
        .tilo_do(json!({"path": "account/workspaces#create",
                        "args": {"name": "Letters"}}))
        .await;
    let workspace = made["result"]["structuredContent"]["result"]["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a workspace is made: {made}"))
        .to_owned();
    let backend = V1 {
        api: &api,
        token: key,
    };

    // The apps, and what one of them holds: the two paths a `tilo ls` takes.
    for path in [None, Some("tasks")] {
        let arguments = match path {
            Some(path) => json!({"workspace": workspace, "path": path}),
            None => json!({"workspace": workspace}),
        };
        let (status, reply) = account
            .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                         "params": {"name": "tilo_ls", "arguments": arguments}}))
            .await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let agent_read = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(!agent_read.is_empty(), "the tool answered lines: {reply}");
        let route = match path {
            Some(path) => format!(
                "/v1/workspaces/{workspace}/resources?path={}",
                tilo_api::client::encode_query(path)
            ),
            None => format!("/v1/workspaces/{workspace}/resources"),
        };
        let (status, listed) = backend.get(&route).await;
        assert_eq!(status, 200, "{listed}");
        for row in listed["rows"].as_array().into_iter().flatten() {
            let name = row["name"].as_str().unwrap_or_default();
            assert!(
                agent_read.contains(name),
                "the API and the tool list the same rows for {path:?}: {name} in {listed}"
            );
        }
    }
}

/// A workspace `person` made, with a key they approved at their `page`.
pub(super) async fn own_workspace(
    on_server: &OnServer,
    person: &SigningKey,
    page: &Device,
) -> String {
    let backend = V1 {
        api: &on_server.api,
        token: approved_key(on_server, person, page, "Visa backend").await,
    };
    let (status, made) = backend
        .account("workspaces#create", &json!({"name": "Ada's notes"}), &[])
        .await;
    assert_eq!(status, 200, "{made}");
    made["workspace"]["id"]
        .as_str()
        .expect("a workspace")
        .to_owned()
}

/// The process holding the session of `person`'s computer `tilo login` made a
/// device of: that one session with the server, entered into `workspace` and
/// the agent `agent`'s session as `tilo login` enters it, and Tilo's API
/// served over it on a socket of its own, as the computer's commands reach it.
pub(super) struct Holder {
    pub(super) device: tilo_session_client::device::Device,
    socket: std::path::PathBuf,
    /// The API the holder serves, as `tilo mcp` asks it.
    direct: Arc<tilo_api::direct::Direct>,
    /// The person's device the computer is.
    pub(super) laptop: Device,
    _sockets: tempfile::TempDir,
}

impl Holder {
    pub(super) async fn start(
        on_server: &OnServer,
        person: &SigningKey,
        workspace: &str,
        agent: &str,
    ) -> Self {
        let laptop = enrol(person, "Ada's laptop", DeviceCapabilities::member());
        let file = tilo_api::direct::KeyFile::new(
            &on_server.server,
            Some(&on_server.web_entry),
            "Ada's laptop",
            &laptop.key,
            &laptop.cert,
        )
        .reaching_at(on_server.hosted.web.host_info().clone());
        let code = format!(
            "https://tilo.sh/#/@{}/w/{workspace}?pair={}",
            on_server.server.trim_start_matches("http://"),
            hex::encode(laptop.cert.user_public_key),
        );
        let signed = tilo_server_client::login::SignIn {
            link: tilo_server_client::login::PairingLink::parse(&code).unwrap(),
            agent: agent.to_owned(),
            label: "Ada's laptop".to_owned(),
            server: on_server.server_host(),
            cert: laptop.cert.clone(),
            server_cert: on_server.host.cert.clone(),
        };
        let device =
            tilo_session_client::device::Device::enter(None, &signed, Arc::new(laptop.key.clone()))
                .await
                .expect("the laptop enters its workspace");
        let direct = Arc::new(tilo_api::direct::Direct::over(&file, device.clone()).unwrap());
        let sockets = tempfile::tempdir().unwrap();
        let socket = sockets.path().join("api.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let serving = Arc::clone(&direct);
        tokio::spawn(tilo_api::direct::serve_current(listener, move || {
            std::future::ready(Ok(Arc::clone(&serving)))
        }));
        Self {
            device,
            socket,
            direct,
            laptop,
            _sockets: sockets,
        }
    }

    /// One command of the computer's, as `tilo` is: a client of the holder's socket.
    pub(super) fn command(&self) -> tilo_api::client::Client {
        tilo_api::client::Client::session(&self.socket)
    }
}

/// A computer `tilo login` made a device of holds that device's one session with its server, and
/// the workspace commands of its login (`tilo do`, `ls`, `read`, `workspace`) are calls to the
/// API the holder serves over that session: each answered, and the session never cut off by a
/// second one under the same key. A script on this computer acts with a key of its own, which
/// the person approved for it (`tilo keys export`): its session and the computer's both stay up
/// while each makes calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_in_computers_commands_go_over_its_one_held_session() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let workspace = own_workspace(&on_server, &ada, &page).await;
    let holder = Holder::start(&on_server, &ada, &workspace, "Claude Code").await;
    let command = || holder.command();
    // The script's key, as `tilo keys export` writes it: one Ada approved for it, reaching where
    // the computer's login reaches, never the computer's own.
    let token = approved_key(&on_server, &ada, &page, "Nightly build").await;
    let file = tilo_api::direct::KeyFile::new(
        &on_server.server,
        Some(&on_server.web_entry),
        "Ada's laptop",
        &holder.laptop.key,
        &holder.laptop.cert,
    )
    .reaching_at(on_server.hosted.web.host_info().clone());
    let script_file = file.clone().with_token(&token).unwrap();
    assert_ne!(script_file.key, file.key);
    let keys = tempfile::tempdir().unwrap();
    let script_path = keys.path().join("script.json");
    std::fs::write(&script_path, serde_json::to_vec(&script_file).unwrap()).unwrap();
    let script = tilo_api::client::Client::open(tilo_api::client::Reach {
        api: None,
        key_file: Some(&script_path),
        json: false,
    })
    .unwrap();
    let mut took = Vec::new();
    for _ in 0..10 {
        let started = std::time::Instant::now();
        let (computer, nothing) = (command(), json!({}));
        let listing = format!("/v1/workspaces/{workspace}/resources");
        let (opened, scripted) = tokio::join!(
            computer.act(&workspace, "tasks", "open", &nothing, "Open Tasks"),
            script.get(&listing),
        );
        let opened = opened.expect("the action answers");
        scripted.expect("the script's own session answers it");
        took.push(started.elapsed());
        assert_ne!(opened["status"], "pending", "{opened}");
        let listed = command()
            .get(&format!("/v1/workspaces/{workspace}/resources"))
            .await
            .unwrap();
        assert!(
            listed["rows"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "{listed}"
        );
    }
    eprintln!("an action over the held session took {took:?}");
    assert!(
        holder.device.is_live(),
        "the device's one session served every command, and the script's session never cut it off"
    );
}

/// The computer's agent, over the session its holder keeps, reads a workspace's people as any
/// key of its person's does, and what it changes there is decided by the grants naming it: its
/// person is asked first, and their answer stands, until they allow it outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_in_computers_agent_reads_and_invites_people_as_its_grants_let_it() {
    use super::api_unattended::{answer, page_in};
    use tilo_action_policy::agent_answers::AnswerScope;
    use tilo_actions::remote::{WORKSPACE_GOVERNANCE_KIND, WORKSPACE_GOVERNANCE_PROVIDER};
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let workspace = own_workspace(&on_server, &ada, &page).await;
    let holder = Holder::start(&on_server, &ada, &workspace, "Claude Code").await;
    let mut ada_at_page = page_in(&on_server, &ada, &workspace).await;

    // `tilo workspace people ls`: reading who is in asks nobody.
    let listed = holder
        .command()
        .post(
            &format!("/v1/workspaces/{workspace}/resources/workspace/members/actions/list"),
            &json!({}),
        )
        .await
        .expect("the agent lists the workspace's people");
    let ada_key = hex::encode(holder.laptop.cert.user_public_key);
    assert!(listed.to_string().contains(&ada_key), "{listed}");
    // `tilo mcp`'s agent reads them the same.
    let read = holder
        .direct
        .mcp(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "tilo_read",
                "arguments": {"path": "workspace/members", "workspace": workspace},
            }}),
        )
        .await
        .expect("the agent's MCP call is answered");
    assert!(
        read.get("error").is_none() && read.to_string().contains(&ada_key),
        "{read}"
    );
    assert!(ada_at_page.asks().await.is_empty(), "reading asks nobody");

    // Inviting someone asks Ada first: her approve at her page lets it through.
    let invitations =
        format!("/v1/workspaces/{workspace}/resources/workspace/invitations/actions/invite");
    let invite = json!({"role": "editor"});
    let asked = holder
        .command()
        .post(&invitations, &invite)
        .await
        .expect("the agent asks Ada");
    assert_eq!(asked["status"], "pending", "{asked}");
    let invocation = asked["invocation"]
        .as_str()
        .unwrap_or_else(|| panic!("Ada is asked: {asked}"))
        .to_owned();
    let question = ada_at_page
        .asked_about(invocation.parse().expect("an invocation id"))
        .await;
    let tilo_actions::remote::asked::AskedBy::Agent(who) = &question.facts.by else {
        panic!("an agent asks: {question:?}");
    };
    assert_eq!(who.name, "Claude Code", "{question:?}");
    assert_eq!(question.workspace_name, workspace, "{question:?}");
    answer(&mut ada_at_page, &invocation, "approve").await;
    let invited = holder
        .command()
        .get(&format!(
            "/v1/workspaces/{workspace}/invocations/{invocation}?wait=30"
        ))
        .await
        .expect("the invite's answer");
    assert!(invited.to_string().contains("http"), "{invited}");

    // Her no stands.
    let asked = holder
        .command()
        .post(&invitations, &invite)
        .await
        .expect("the agent asks Ada again");
    assert_eq!(asked["status"], "pending", "{asked}");
    let invocation = asked["invocation"].as_str().unwrap().to_owned();
    answer(&mut ada_at_page, &invocation, "decline").await;
    let refused = holder
        .command()
        .get(&format!(
            "/v1/workspaces/{workspace}/invocations/{invocation}?wait=30"
        ))
        .await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| format!("{error:#}").contains("did not allow"))
            || refused
                .as_ref()
                .is_ok_and(|refused| refused.to_string().contains("did not allow")),
        "{refused:?}"
    );

    // Allowed in the workspace's settings, under Agents, it goes unasked: Ada's laptop sets the
    // agent's bound in the workspace's list of its agents, as her settings do, and the server
    // decides the agent's calls by it.
    let workspaces = on_server.hosted.server.workspaces().unwrap();
    let invite_node = AnswerScope::action(
        WORKSPACE_GOVERNANCE_PROVIDER,
        WORKSPACE_GOVERNANCE_KIND,
        "invite",
    )
    .node(&workspace);
    let agent =
        tilo_actions::remote::device_agent_id(&holder.laptop.cert.user_public_key, "Claude Code");
    allow_on_list(workspaces, &holder, (&workspace, &agent), &invite_node).await;
    let invited = holder
        .command()
        .post(&invitations, &invite)
        .await
        .expect("allowed outright, the agent invites");
    assert!(invited.to_string().contains("http"), "{invited}");
    assert!(
        ada_at_page.asks().await.is_empty(),
        "allowed outright, nobody is asked"
    );
    assert!(holder.device.is_live());
}

/// Ada's laptop allows `agent` outright at `node` in `workspace`: it sets the agent's bound in the
/// workspace's list of its agents as her own write, once the list names the agent, and waits
/// until the server holds it.
async fn allow_on_list(
    workspaces: &tilo_server::workspaces::Workspaces,
    holder: &Holder,
    (workspace, agent): (&str, &str),
    node: &tilo_action_policy::TreePath,
) {
    use ui::tilo_workspace_agent::{Bound, WorkspaceAgents, workspace_agents::SetAgentBound};
    let chat = ui::tilo_workspace_agent::OWNER;
    let document = &collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let listed = |bounded: bool| async move {
        let bytes = workspaces
            .resource_document(workspace, chat, document)
            .await
            .ok()
            .flatten()?;
        let doc = automerge::AutoCommit::load(&bytes).ok()?;
        let agents = <WorkspaceAgents as ui::tilo_resource::Resource>::read(&doc).ok()?;
        agents
            .agents
            .iter()
            .any(|kept| kept.id.0 == agent && (!bounded || !kept.bound.is_empty()))
            .then_some(doc)
    };
    let deadline = tilo_test_env::hang_deadline();
    let doc = loop {
        if let Some(doc) = listed(false).await {
            break doc;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the workspace's agents never list the computer's agent"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let ada = hex::encode(holder.laptop.cert.user_public_key);
    let mut list = ui::tilo_resource::Replica::<WorkspaceAgents>::from_doc(doc)
        .as_author(&ada)
        .in_app(chat, "Chat");
    let heads = list.doc_mut().get_heads();
    list.apply(WorkspaceAgents::SET_AGENT_BOUND.with(SetAgentBound {
        agent: ui::tilo_resource::Id::from_raw(agent.to_owned()),
        bound: vec![Bound {
            node: node.to_string(),
            kinds: vec!["outside".into()],
            answer: "allow".into(),
        }],
    }))
    .expect("its person may allow it to invite people");
    let changes = list.doc_mut().save_after(&heads);
    // Her laptop's session holds the change, seals it under the workspace's keys once they are
    // here, and sends it to the workspace as each of her writes there.
    tilo_session_client::Outbox::write(
        holder.device.session(),
        tilo_session_client::DocumentRoute {
            cluster_id: holder.device.workspace_id().to_string(),
            app_id: chat.to_owned(),
            document: document.clone(),
        },
        &collab_types::DocumentMutation::Incremental(changes),
    )
    .expect("Ada's laptop writes the bound");
    while listed(true).await.is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the server never holds the agent's bound"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// What a hosted workspace answers a key besides actions: what changed in it,
/// as it changes; search over what it holds; and a run taken back whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_workspace_streams_changes_finds_and_undoes_runs() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let backend = V1 {
        api: &api,
        token: approved_key(&on_server, &ada, &page, "Notes backend").await,
    };
    let (status, made) = backend
        .account("workspaces#create", &json!({"name": "Letters"}), &[])
        .await;
    assert_eq!(status, 200, "{made}");
    let base = format!(
        "/v1/workspaces/{}",
        made["workspace"]["id"].as_str().unwrap()
    );
    let mut stream = backend
        .send(hyper::Method::GET, &format!("{base}/events"), b"", &[])
        .await
        .into_body();

    let run_headers = [
        ("tilo-invocation", "letter"),
        ("tilo-invocation-title", "Write the letter"),
    ];
    let (status, opened) = backend
        .post(
            &format!("{base}/resources/word-processor/actions/open"),
            &json!({}),
            &run_headers,
        )
        .await;
    assert_eq!(status, 200, "{opened}");
    let (status, written) = backend
        .post(
            &format!("{base}/resources/word-processor/actions/append-paragraph"),
            &json!({"text": "Gooseberry jam for the embassy"}),
            &run_headers,
        )
        .await;
    assert_eq!(status, 200, "{written}");
    let run = written["invocation"].as_str().unwrap().to_owned();

    // Search finds what the workspace holds: the workspace's search is an action of its own.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let found = loop {
        let (status, found) = backend
            .post(
                &format!("{base}/resources/search/actions/find"),
                &json!({"query": "Gooseberry"}),
                &[],
            )
            .await;
        assert_eq!(status, 200, "a hosted workspace answers search: {found}");
        let found = found["result"].clone();
        if found["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty())
            || tokio::time::Instant::now() > deadline
        {
            break found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    };
    assert!(
        found["hits"].to_string().contains("word-processor"),
        "{found}"
    );

    // The run is taken back whole, by its id.
    let (status, undone) = backend
        .post(
            &format!("{base}/invocations/{run}/undo"),
            &json!({}),
            &[("idempotency-key", "undo-letter")],
        )
        .await;
    assert_eq!((status, &undone["status"]), (200, &json!("ok")), "{undone}");
    assert!(
        undone["undone"]
            .as_array()
            .is_some_and(|undone| !undone.is_empty()),
        "{undone}"
    );
    let (_, document) = backend
        .get(&format!("{base}/resources/word-processor"))
        .await;
    assert!(
        !document.to_string().contains("Gooseberry"),
        "the paragraph is gone: {document}"
    );

    // Record changes come down the stream, not only runs.
    let mut seen = String::new();
    let within = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !seen.contains("event: change") {
        let frame = tokio::time::timeout_at(within, stream.frame())
            .await
            .unwrap_or_else(|_| panic!("a change comes down the stream within 30 s: {seen}"))
            .expect("the stream stays open")
            .unwrap();
        if let Ok(data) = frame.into_data() {
            seen.push_str(&String::from_utf8_lossy(&data));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_larger_than_one_request_goes_up_in_pieces_that_resume() {
    let served = api_on_server(ISSUER).await;
    let api = Arc::clone(&served.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let backend = V1 {
        api: &api,
        token: approved_key(&served, &ada, &page, "Visa backend").await,
    };
    let (_, made) = backend
        .account("workspaces#create", &json!({"name": "Contracts"}), &[])
        .await;
    // A file of the size one request would not carry, its bytes all
    // different so a piece lost or doubled shows.
    let whole: Vec<u8> = (0..5_000_000_u32).map(|at| (at % 251) as u8).collect();
    let hash = blake3::hash(&whole).to_hex().to_string();
    let base = format!(
        "/v1/workspaces/{}/contents/{hash}",
        made["workspace"]["id"].as_str().expect("a workspace")
    );
    let piece = |from: usize, of: usize| whole[from..(from + of).min(whole.len())].to_vec();
    let range = |from: usize, of: usize| {
        let to = (from + of).min(whole.len()) - 1;
        format!("bytes {from}-{to}/{}", whole.len())
    };

    // The first piece opens the put where it starts and says how big the file
    // is; the answer says where the next one goes.
    let first = piece(0, 1_000_000);
    let first_range = range(0, 1_000_000);
    let (status, access) = json_of(
        backend
            .send(
                hyper::Method::PUT,
                &base,
                &first,
                &[("content-range", first_range.as_str())],
            )
            .await,
    )
    .await;
    assert_eq!(status, 200, "{access}");
    assert_eq!(access["offset"], json!(1_000_000), "{access}");
    assert_eq!(access["size"], json!(whole.len()), "{access}");

    // The same piece sent again after an answer that did not arrive costs
    // nothing: the answer says where the put stands now.
    let (status, again) = json_of(
        backend
            .send(
                hyper::Method::PUT,
                &base,
                &first,
                &[("content-range", first_range.as_str())],
            )
            .await,
    )
    .await;
    assert_eq!(
        (status, &again["offset"]),
        (200, &json!(1_000_000)),
        "{again}"
    );

    // A piece from an offset the put is not at is refused with where it stands.
    let ahead = range(2_000_000, 1_000_000);
    let (status, refused) = json_of(
        backend
            .send(
                hyper::Method::PUT,
                &base,
                &piece(2_000_000, 1_000_000),
                &[("content-range", ahead.as_str())],
            )
            .await,
    )
    .await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (409, &json!("offset_mismatch")),
        "{refused}"
    );

    // After a connection that stopped, the next piece continues from where
    // the put stands, and the last one answers the content the whole file is
    // kept under.
    let mut at = 1_000_000;
    let mut content = Value::Null;
    while at < whole.len() {
        let chunk = piece(at, 1_000_000);
        let from = at;
        let content_range = range(from, chunk.len());
        let (status, answered) = json_of(
            backend
                .send(
                    hyper::Method::PUT,
                    &base,
                    &chunk,
                    &[("content-range", content_range.as_str())],
                )
                .await,
        )
        .await;
        assert!(status == 200 || status == 201, "{answered}");
        at += chunk.len();
        assert_eq!(
            answered["content"].is_string(),
            at == whole.len(),
            "{answered}"
        );
        if at == whole.len() {
            content = answered["content"].clone();
        }
    }
    let content = content
        .as_str()
        .expect("the put names its content")
        .to_owned();
    assert!(content.starts_with("tilo.content:"), "{content}");
    let reference = content
        .strip_prefix("tilo.content:")
        .expect("a content reference");
    let read = format!(
        "/v1/workspaces/{}/contents/{reference}",
        made["workspace"]["id"].as_str().unwrap()
    );

    // Every byte of it, byte for byte.
    let reply = backend.send(hyper::Method::GET, &read, b"", &[]).await;
    let body = reply.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), whole.as_slice());

    // A piece sent after every byte is there is told so.
    let last_range = range(whole.len() - 1_000_000, 1_000_000);
    let (status, complete) = json_of(
        backend
            .send(
                hyper::Method::PUT,
                &base,
                &piece(whole.len() - 1_000_000, 1_000_000),
                &[("content-range", last_range.as_str())],
            )
            .await,
    )
    .await;
    assert_eq!(
        (status, &complete["error"]["code"]),
        (409, &json!("upload_complete")),
        "{complete}"
    );
}

/// An API server before two servers forwards a key's calls to the one that keeps its person's
/// workspaces, though the other answers too. The other one lets any certified device
/// connect, as a server that keeps workspaces for the people it lets in does, and keeps nothing
/// of this person's: asked for her workspace it knows none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_reaches_the_server_that_keeps_the_workspace_though_another_answers() {
    Box::pin(async {
        let hers = api_on_server(ISSUER).await;
        let other = api_on_server(ISSUER).await;
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
        let token = approved_key(&hers, &ada, &page, "Visa backend").await;
        let home = V1 {
            api: &hers.api,
            token: token.clone(),
        };
        let (status, made) = home
            .account("workspaces#create", &json!({"name": "Contracts"}), &[])
            .await;
        assert_eq!(status, 200, "{made}");
        let workspace = made["workspace"]["id"]
            .as_str()
            .expect("a workspace")
            .to_owned();

        // What the test rests on: the other server lets her key's device connect and lists
        // nothing for it, and her own keeps the workspace.
        let key = tilo_api::store::Store::in_memory()
            .access(&token, tilo_clock::unix_now_ms())
            .expect("the token carries its grant")
            .key()
            .expect("and the grant its key");
        let other_host = Arc::new(tilo_api::server::Server::new(
            other.hosted.web.host_info().clone(),
        ));
        let session = other_host
            .connect(&key)
            .await
            .expect("the other server lets the key's device connect");
        let listed = other_host.workspaces(&session).await.unwrap_or_default();
        assert!(
            listed.is_empty(),
            "the other server keeps none of her workspaces: {listed:?}"
        );
        drop(session);
        let her_host = Arc::new(tilo_api::server::Server::new(
            hers.hosted.web.host_info().clone(),
        ));
        let session = her_host
            .connect(&key)
            .await
            .expect("her own server is reached");
        drop(session);

        let front = Server::new(
            &tilo_api::Config {
                issuer: ISSUER.into(),
                web_entry: hers.web_entry.clone(),
                hosts: vec![
                    other.hosted.web.host_info().clone(),
                    hers.hosted.web.host_info().clone(),
                ],
            },
            tilo_api::store::Store::in_memory(),
        )
        .unwrap();
        let through = V1 { api: &front, token };
        // The workspace's own governance, which the server keeping it answers.
        let (status, held) = through
            .post(
                &format!("/v1/workspaces/{workspace}/resources/workspace/servers/actions/list"),
                &serde_json::json!({}),
                &[],
            )
            .await;
        assert_eq!(
            status, 200,
            "a call for her workspace reaches the server that keeps it: {held}"
        );
        let (status, listed) = through.account("workspaces#list", &json!({}), &[]).await;
        assert_eq!(status, 200, "{listed}");
        assert!(
            listed["workspaces"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == workspace.as_str())),
            "her workspaces are listed where they are kept: {listed}"
        );
    })
    .await;
}

/// An API server that never approved the key serves it anyway: the key is the
/// grant the person signed, so every API server reads the same one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_works_at_every_api_server() {
    let served = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&served, &ada, &page, "Visa backend").await;

    let second = Server::new(
        &tilo_api::Config {
            issuer: ISSUER.into(),
            web_entry: served.web_entry.clone(),
            hosts: vec![served.hosted.web.host_info().clone()],
        },
        tilo_api::store::Store::in_memory(),
    )
    .unwrap();
    let elsewhere = V1 {
        api: &second,
        token: token.clone(),
    };
    let (status, listed) = elsewhere.account("workspaces#list", &json!({}), &[]).await;
    assert_eq!(status, 200, "{listed}");
    assert!(listed["workspaces"].is_array(), "{listed}");

    // The API server that approved it serves the same key, and the same grant.
    let home = V1 {
        api: &served.api,
        token,
    };
    let (status, listed) = home.account("workspaces#list", &json!({}), &[]).await;
    assert_eq!(status, 200, "{listed}");
}

/// The server a call used goes away; another server of the person's serves the
/// same key, because the key is the grant and every server admits it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_served_after_the_host_it_used_goes_away() {
    Box::pin(async {
        let on_server = api_on_server(ISSUER).await;
        let used = on_server.hosted.web.host_info().clone();
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
        let token = approved_key(&on_server, &ada, &page, "Visa backend").await;
        let home = V1 {
            api: &on_server.api,
            token: token.clone(),
        };
        let (status, listed) = home.account("workspaces#list", &json!({}), &[]).await;
        assert_eq!(status, 200, "{listed}");

        // The server the call used stops serving; another takes the key's calls.
        let restarted = on_server.restart(true).await;
        let second = Server::new(
            &tilo_api::Config {
                issuer: ISSUER.into(),
                web_entry: restarted.web_entry.clone(),
                hosts: vec![used, restarted.hosted.web.host_info().clone()],
            },
            tilo_api::store::Store::in_memory(),
        )
        .unwrap();
        let elsewhere = V1 {
            api: &second,
            token,
        };
        let (status, listed) = elsewhere.account("workspaces#list", &json!({}), &[]).await;
        assert_eq!(status, 200, "{listed}");
    })
    .await;
}

/// Nothing that can read the workspace is online: the call is refused in
/// plain words, with what to do about it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_host_that_can_read_the_workspace_is_a_typed_refusal() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &page, "Visa backend").await;

    let nowhere = Server::new(
        &tilo_api::Config {
            issuer: ISSUER.into(),
            web_entry: on_server.web_entry.clone(),
            // Nothing serves at this address: no server is online for the key.
            hosts: vec![{
                let mut dead = on_server.hosted.web.host_info().clone();
                dead.address.as_mut().expect("a host").endpoint_addr =
                    iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x5a; 32]).public())
                        .with_ip_addr("127.0.0.1:9".parse().expect("an address"));
                dead
            }],
        },
        tilo_api::store::Store::in_memory(),
    )
    .unwrap();
    let backend = V1 {
        api: &nowhere,
        token,
    };
    let (status, refused) = backend.account("workspaces#list", &json!({}), &[]).await;
    assert_eq!(status, 503, "{refused}");
    assert_eq!(refused["error"]["code"], "unavailable", "{refused}");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("No device that can read this workspace is online"),
        "{refused}"
    );
    assert!(
        refused["error"]["next_steps"]
            .as_array()
            .is_some_and(|steps| !steps.is_empty()),
        "{refused}"
    );
}

/// What a server answers `device` asking `target` in the workspace `id`: the answer, or the
/// code of its refusal.
async fn asked(
    device: &tilo_session_client::device::Device,
    id: WorkspaceId,
    target: RemoteActionTarget,
    argument: &Value,
) -> Result<Value, String> {
    let arguments = argument.as_object().cloned().unwrap_or_default();
    device
        .ask_host(id, target, arguments)
        .await
        .map_err(|refused| refused.code)
}

/// What the workspace keeps for its members, each with what it is called here: its vault listed
/// and added to, and a model key set.
fn what_the_workspace_keeps() -> [(&'static str, RemoteActionTarget, Value); 3] {
    [
        (
            "lists the vault",
            person_vault::target(person_vault::LIST),
            json!({}),
        ),
        (
            "adds to the vault",
            person_vault::target(person_vault::ADD),
            json!({
                "kind": "login",
                "belongsTo": ["https://mail.example.test"],
                "username": "ada@example.test",
                "secret": "correct horse battery staple",
            }),
        ),
        (
            "sets a model key",
            person_models::target(person_models::SET_KEY),
            json!({"provider": "a-provider", "key": "sk-test-not-a-key"}),
        ),
    ]
}

/// What the workspace keeps (what its members sign in with, their model keys) answers its
/// members on devices of theirs, and a key a person made for a script as their agent there: the
/// preset their approval of the key gave its agent is what it reaches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_made_for_a_script_reaches_the_workspaces_stores_as_its_persons_agent() {
    Box::pin(async {
        let served = api_on_server(ISSUER).await;
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
        // The key as `tilo keys create` makes one: asked of the API, approved by Ada on her page.
        let token = approved_key(&served, &ada, &page, "Nightly build").await;
        let backend = V1 {
            api: &served.api,
            token: token.clone(),
        };
        let (status, made) = backend
            .account("workspaces#create", &json!({"name": "Ada's own"}), &[])
            .await;
        assert_eq!(status, 200, "{made}");
        let workspace = made["workspace"]["id"]
            .as_str()
            .expect("a workspace")
            .to_owned();

        let holder = Arc::new(tilo_api::server::Server::new(
            served.hosted.web.host_info().clone(),
        ));
        let now_ms = tilo_clock::unix_now_ms();
        let list = || person_vault::target(person_vault::LIST);

        // Ada's laptop is a device she paired: its certificate is no grant, and each of these
        // is hers to ask there.
        let laptop = enrol(&ada, "Ada's laptop", DeviceCapabilities::member());
        let paired = tilo_api::grant_key::GrantKey::held(
            laptop.key.clone(),
            laptop.cert.clone(),
            now_ms.saturating_mul(1_000_000),
        )
        .expect("a paired device's own key");
        let on_laptop = holder
            .connect(&paired)
            .await
            .expect("her laptop reaches her server");
        let laptop_in = holder
            .enter(&on_laptop, &workspace)
            .await
            .expect("her laptop enters her workspace");
        for (what, target, argument) in what_the_workspace_keeps() {
            let answered = asked(&on_laptop.device, laptop_in, target, &argument).await;
            assert_ne!(
                answered.as_ref().err().map(String::as_str),
                Some("unknown_action"),
                "Ada, on a device she paired, {what}"
            );
        }
        let kept = asked(&on_laptop.device, laptop_in, list(), &json!({}))
            .await
            .expect("her own device lists the workspace's vault");
        assert_eq!(kept["items"].as_array().map(Vec::len), Some(1), "{kept}");

        // The key itself, read from its token as every API server reads it: a server key, which
        // is an agent's grant reaching all her workspaces.
        let key = tilo_api::store::Store::in_memory()
            .access(&token, now_ms)
            .expect("the token carries its grant")
            .key()
            .expect("and the grant its key");
        let certified = &key.cert().capabilities;
        assert!(certified.server_key && certified.agent.is_some());
        let with_key = holder
            .connect(&key)
            .await
            .expect("the key reaches the server");
        let key_in = holder
            .enter(&with_key, &workspace)
            .await
            .expect("the key enters the workspace it made");
        // The key reads the same store: it is her agent in the workspace, and what its approval
        // gave that agent is what an agent of hers may.
        let listed = asked(&with_key.device, key_in, list(), &json!({}))
            .await
            .expect("her key, as her agent, lists the workspace's vault");
        assert_eq!(
            listed, kept,
            "the vault the workspace keeps, as she reads it"
        );
        for (what, target, argument) in what_the_workspace_keeps().into_iter().skip(1) {
            let answered = asked(&with_key.device, key_in, target, &argument).await;
            assert_ne!(
                answered.as_ref().err().map(String::as_str),
                Some("unknown_action"),
                "her key, as her agent, {what}"
            );
        }

        // Her own device still reads the vault the workspace keeps, whatever her key asked.
        let after = asked(&on_laptop.device, laptop_in, list(), &json!({}))
            .await
            .expect("her own device lists the workspace's vault");
        assert!(
            after["items"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "{after}"
        );
    })
    .await;
}
