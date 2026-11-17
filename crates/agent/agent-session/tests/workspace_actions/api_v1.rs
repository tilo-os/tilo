//! The API's `/v1` routes against a workspace's server, as tilo.sh runs them: a
//! person's key makes a workspace (a retry of the same request makes no
//! second one), puts a form in, runs actions on it, follows its runs and
//! reads the workspace's events as they happen, has them posted to a webhook
//! (signed, retried until taken), mints a key for that workspace alone, and
//! deletes the workspace. Every route is one of the calls an agent makes
//! over MCP, so this is that same path through HTTP and JSON.

use super::api::ISSUER;
use super::api_on_relay::{api_on_relay, approved_key, headers, json_of};
use super::hosted_documents::application_pdf;
use super::*;
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_actions::remote::{person_accounts, person_models, person_vault};
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
}

/// A webhook's receiver on loopback: each delivery's headers and body, the
/// first one answered 500 so the server must try it again.
async fn receiver() -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<(hyper::HeaderMap, Vec<u8>)>,
) {
    use hyper::service::service_fn;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/hook",
        listener.local_addr().unwrap().port()
    );
    let (sender, received) = tokio::sync::mpsc::unbounded_channel();
    let refused_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let sender = sender.clone();
            let refused_once = Arc::clone(&refused_once);
            tokio::spawn(async move {
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let sender = sender.clone();
                    let refused_once = Arc::clone(&refused_once);
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes().to_vec();
                        let first = !refused_once.swap(true, std::sync::atomic::Ordering::SeqCst);
                        let _ = sender.send((parts.headers, body));
                        let mut answer = hyper::Response::new(http_body_util::Full::new(
                            hyper::body::Bytes::new(),
                        ));
                        if first {
                            *answer.status_mut() = hyper::StatusCode::INTERNAL_SERVER_ERROR;
                        }
                        Ok::<_, std::convert::Infallible>(answer)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (url, received)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_works_through_the_api_on_a_relay_host() {
    // The story builds a host, its API and a workspace's sessions; its future goes on the
    // heap rather than into the frames of the test thread that polls it.
    Box::pin(a_backend_key_works_through_the_api_on_a_relay_host()).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "one backend's story through the API, from its first workspace to deleting it"
)]
async fn a_backend_key_works_through_the_api_on_a_relay_host() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let backend = V1 {
        api: &api,
        token: approved_key(&on_relay, &ada, &page, "Visa backend").await,
    };

    // The API says its version, and what the key reaches.
    let (status, info) = backend.get("/v1").await;
    assert_eq!((status, &info["api_version"]), (200, &json!("1")), "{info}");
    assert_eq!(info["key"]["scope"], "all", "{info}");

    // A workspace, made once however often the same request is retried.
    let new = json!({"name": "Applicant: Jane Doe"});
    let retry = [("idempotency-key", "make-jane")];
    let (status, made) = backend.post("/v1/workspaces", &new, &retry).await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();
    let replayed = backend
        .send(
            hyper::Method::POST,
            "/v1/workspaces",
            new.to_string().as_bytes(),
            &retry,
        )
        .await;
    assert_eq!(replayed.headers()["idempotent-replayed"], "true");
    let (_, again) = json_of(replayed).await;
    assert_eq!(
        again["id"], made["id"],
        "a retry answers the first workspace"
    );
    let (status, conflict) = backend
        .post("/v1/workspaces", &json!({"name": "Someone else"}), &retry)
        .await;
    assert_eq!(
        (status, &conflict["error"]["code"]),
        (409, &json!("idempotency_conflict"))
    );
    let (status, listed) = backend.get("/v1/workspaces").await;
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
    let uploaded = backend
        .send(
            hyper::Method::POST,
            &format!("{base}/uploads"),
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
    // And posted to a webhook, signed.
    let (url, mut deliveries) = receiver().await;
    let (status, hook) = backend
        .post(
            &format!("{base}/webhooks"),
            &json!({"url": url, "events": ["invocation"]}),
            &[],
        )
        .await;
    assert_eq!(status, 201, "{hook}");
    let secret = hook["secret"].as_str().unwrap().to_owned();

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
    while !seen.contains("event: run") {
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
        seen.contains("\"run\":"),
        "a run event names its run: {seen}"
    );

    // The webhook was refused once, and the same delivery came again.
    let (first_headers, first_body) =
        tokio::time::timeout(std::time::Duration::from_secs(60), deliveries.recv())
            .await
            .expect("a run reached the webhook")
            .unwrap();
    let (again_headers, again_body) =
        tokio::time::timeout(std::time::Duration::from_secs(30), deliveries.recv())
            .await
            .expect("the refused delivery is tried again")
            .unwrap();
    assert_eq!(
        first_headers["tilo-delivery"],
        again_headers["tilo-delivery"]
    );
    assert_eq!(first_body, again_body);
    let signed = again_headers["tilo-signature"].to_str().unwrap();
    let time: u64 = signed
        .strip_prefix("t=")
        .and_then(|rest| rest.split(',').next())
        .and_then(|time| time.parse().ok())
        .unwrap();
    assert_eq!(
        signed,
        tilo_api::v1::webhooks::signature(&secret, time, &again_body),
        "the delivery is signed with the webhook's secret"
    );
    let event: Value = serde_json::from_slice(&again_body).unwrap();
    assert_eq!(
        (&event["type"], &event["workspace"]),
        (&json!("run"), &json!(workspace)),
        "{event}"
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
        .post(
            "/v1/keys",
            &json!({"workspace": workspace, "name": "Jane's agent"}),
            &[],
        )
        .await;
    assert_eq!(status, 201, "{minted}");
    let agent = V1 {
        api: &api,
        token: minted["token"].as_str().unwrap().to_owned(),
    };
    let (status, listing) = agent.get(&format!("{base}/resources")).await;
    assert_eq!(status, 200, "{listing}");
    // A listing is its rows once: the words a tool says as its typed answer
    // are that answer again, and are no text of it.
    assert!(
        listing["text"]
            .as_str()
            .is_none_or(|text| serde_json::from_str::<serde_json::Value>(text).is_err()),
        "the listing carries itself as its text: {listing}"
    );
    let (status, others) = agent
        .get(&format!("/v1/workspaces/{}/resources", on_relay.lobby))
        .await;
    assert_eq!(
        (status, &others["error"]["code"]),
        (403, &json!("beyond_scope")),
        "{others}"
    );
    let (status, minting) = agent
        .post("/v1/keys", &json!({"workspace": workspace}), &[])
        .await;
    assert!(status >= 400, "a minted key mints none: {minting}");

    // The workspace records the key it admitted, so an API server that never
    // saw the approval lists it from the workspace itself.
    let second = Server::new(
        &tilo_api::Config {
            issuer: ISSUER.into(),
            web_entry: on_relay.web_entry.clone(),
            hosts: vec![on_relay.relay.clone()],
        },
        tilo_api::store::Store::in_memory(),
    )
    .unwrap();
    let elsewhere = V1 {
        api: &second,
        token: minted["token"].as_str().unwrap().to_owned(),
    };
    let (status, listed) = elsewhere.get("/v1/keys").await;
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
                hyper::Method::DELETE,
                &format!("/v1/keys/{}", minted["grant"]),
                b"",
                &[("idempotency-key", "revoke-jane")],
            )
            .await,
    )
    .await;
    assert_eq!(status, 200, "{revoked}");
    let (status, refused) = elsewhere.get("/v1/keys").await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (401, &json!("key_revoked")),
        "{refused}"
    );

    // Deleting the workspace revokes the key that reached only it.
    let (status, minted) = backend
        .post(
            "/v1/keys",
            &json!({"workspace": workspace, "name": "Jane's second agent"}),
            &[],
        )
        .await;
    assert_eq!(status, 201, "{minted}");
    let agent = V1 {
        api: &api,
        token: minted["token"].as_str().unwrap().to_owned(),
    };
    let deleted = backend
        .send(
            hyper::Method::DELETE,
            &base,
            b"",
            &[("idempotency-key", "drop-jane")],
        )
        .await;
    let (status, deleted) = json_of(deleted).await;
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

/// A program holding a key of the person's own (a device of theirs, not a
/// grant an API server keeps) reaches the host directly: the API served in
/// process, the same routes and answers, with no API server between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_its_program_holds_reaches_the_host_directly() {
    let on_relay = api_on_relay(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    // Ada comes to the relay once, as every person does.
    approved_key(&on_relay, &ada, &page, "Visa backend").await;
    let laptop = enrol(&ada, "Ada's script", DeviceCapabilities::member());
    let file = tilo_api::direct::KeyFile::new(
        &on_relay.relay,
        Some(&on_relay.web_entry),
        "Ada's script",
        &laptop.key,
        &laptop.cert,
    );
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
            .request("POST", "/v1/workspaces", &retried, new.as_bytes())
            .await,
    )
    .await;
    assert_eq!(status, 201, "{made}");
    let (_, again) = json_of(
        direct
            .request("POST", "/v1/workspaces", &retried, new.as_bytes())
            .await,
    )
    .await;
    assert_eq!(again["id"], made["id"], "a retry makes no second workspace");
    let workspace = made["id"].as_str().unwrap();
    let (status, listed) = json_of(direct.request("GET", "/v1/workspaces", &[], b"").await).await;
    assert_eq!(status, 200, "{listed}");
    assert!(listed.to_string().contains(workspace), "{listed}");
    let (status, opened) = json_of(
        direct
            .request(
                "POST",
                &format!("/v1/workspaces/{workspace}/resources/notes/actions/open"),
                &json,
                b"{}",
            )
            .await,
    )
    .await;
    assert_eq!(
        status, 200,
        "the host's app workers answer the key: {opened}"
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

/// What a hosted workspace answers a key besides actions: what changed in it,
/// as it changes; search over what it holds; and a run taken back whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_workspace_streams_changes_finds_and_undoes_runs() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let backend = V1 {
        api: &api,
        token: approved_key(&on_relay, &ada, &page, "Notes backend").await,
    };
    let (status, made) = backend
        .post("/v1/workspaces", &json!({"name": "Letters"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let base = format!("/v1/workspaces/{}", made["id"].as_str().unwrap());
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

    // Search finds what the workspace holds.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let found = loop {
        let (status, found) = backend.get(&format!("{base}/search?q=Gooseberry")).await;
        assert_eq!(status, 200, "a hosted workspace answers search: {found}");
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
    let served = api_on_relay(ISSUER).await;
    let api = Arc::clone(&served.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let backend = V1 {
        api: &api,
        token: approved_key(&served, &ada, &page, "Visa backend").await,
    };
    let (_, made) = backend
        .post("/v1/workspaces", &json!({"name": "Contracts"}), &[])
        .await;
    let base = format!(
        "/v1/workspaces/{}/uploads",
        made["id"].as_str().expect("a workspace")
    );
    // A file of the size one request would not carry, its bytes all
    // different so a piece lost or doubled shows.
    let whole: Vec<u8> = (0..5_000_000_u32).map(|at| (at % 251) as u8).collect();
    let id = "up_0123456789abcdef0123456789abcdef";
    let upload = format!("{base}/{id}");

    // The upload is opened with its size, and opening it again answers where
    // it stands rather than opening a second one.
    let (status, opened) = backend
        .post(&upload, &json!({"size": whole.len()}), &[])
        .await;
    assert_eq!(status, 201, "{opened}");
    assert_eq!(opened["offset"], json!(0), "{opened}");
    let (status, again) = backend
        .post(&upload, &json!({"size": whole.len()}), &[])
        .await;
    assert_eq!((status, &again["offset"]), (200, &json!(0)), "{again}");

    // The first piece goes at the offset it stands at, and the answer says
    // where the next one does.
    let piece = |from: usize, of: usize| whole[from..(from + of).min(whole.len())].to_vec();
    let first = piece(0, 1_000_000);
    let reply = backend
        .send(
            hyper::Method::PUT,
            &format!("{upload}?offset=0"),
            &first,
            &[],
        )
        .await;
    let (status, access) = json_of(reply).await;
    assert_eq!(status, 200, "{access}");
    assert_eq!(access["offset"], json!(1_000_000), "{access}");

    // A piece sent again from where it already stands is refused, with the
    // offset to send from: the workspace never keeps it twice.
    let reply = backend
        .send(
            hyper::Method::PUT,
            &format!("{upload}?offset=0"),
            &first,
            &[],
        )
        .await;
    let (status, refused) = json_of(reply).await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (409, &json!("offset_mismatch")),
        "{refused}"
    );
    let (status, access) = backend.get(&upload).await;
    assert_eq!((status, &access["offset"]), (200, &json!(1_000_000)));

    // After a connection that stopped, the next piece continues from where
    // the workspace says the upload stands, and the last one answers the
    // content the whole file is kept under.
    let mut at = access["offset"].as_u64().unwrap() as usize;
    let mut content = Value::Null;
    while at < whole.len() {
        let chunk = piece(at, 1_000_000);
        let reply = backend
            .send(
                hyper::Method::PUT,
                &format!("{upload}?offset={at}"),
                &chunk,
                &[],
            )
            .await;
        let (status, answered) = json_of(reply).await;
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
    // Every byte of it, byte for byte, and nothing of the upload left open.
    let reply = backend
        .send(
            hyper::Method::GET,
            &format!(
                "/v1/workspaces/{}/content/{}",
                made["id"].as_str().unwrap(),
                content.as_str().unwrap()
            ),
            b"",
            &[],
        )
        .await;
    let body = reply.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), whole.as_slice());
    let (status, gone) = backend.get(&upload).await;
    assert_eq!(
        (status, &gone["error"]["code"]),
        (404, &json!("not_found")),
        "{gone}"
    );
}

/// An API server that never approved the key serves it anyway: the key is the
/// grant the person signed, so every API server reads the same one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_works_at_every_api_server() {
    let on_relay = api_on_relay(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_relay, &ada, &page, "Visa backend").await;

    let second = Server::new(
        &tilo_api::Config {
            issuer: ISSUER.into(),
            web_entry: on_relay.web_entry.clone(),
            hosts: vec![on_relay.relay.clone()],
        },
        tilo_api::store::Store::in_memory(),
    )
    .unwrap();
    let elsewhere = V1 {
        api: &second,
        token: token.clone(),
    };
    let (status, listed) = elsewhere.get("/v1/workspaces").await;
    assert_eq!(status, 200, "{listed}");
    assert!(listed["workspaces"].is_array(), "{listed}");

    // The API server that approved it serves the same key, and the same grant.
    let home = V1 {
        api: &on_relay.api,
        token,
    };
    let (status, listed) = home.get("/v1/workspaces").await;
    assert_eq!(status, 200, "{listed}");
}

/// The host a call used goes away; another host of the person's serves the
/// same key, because the key is the grant and every host admits it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_served_after_the_host_it_used_goes_away() {
    Box::pin(async {
        let on_relay = api_on_relay(ISSUER).await;
        let used = on_relay.relay.clone();
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
        let token = approved_key(&on_relay, &ada, &page, "Visa backend").await;
        let home = V1 {
            api: &on_relay.api,
            token: token.clone(),
        };
        let (status, listed) = home.get("/v1/workspaces").await;
        assert_eq!(status, 200, "{listed}");

        // The host the call used stops serving; another takes the key's calls.
        let restarted = on_relay.restart(true).await;
        let second = Server::new(
            &tilo_api::Config {
                issuer: ISSUER.into(),
                web_entry: restarted.web_entry.clone(),
                hosts: vec![used, restarted.relay.clone()],
            },
            tilo_api::store::Store::in_memory(),
        )
        .unwrap();
        let elsewhere = V1 {
            api: &second,
            token,
        };
        let (status, listed) = elsewhere.get("/v1/workspaces").await;
        assert_eq!(status, 200, "{listed}");
    })
    .await;
}

/// Nothing that can read the workspace is online: the call is refused in
/// plain words, with what to do about it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_host_that_can_read_the_workspace_is_a_typed_refusal() {
    let on_relay = api_on_relay(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_relay, &ada, &page, "Visa backend").await;

    let nowhere = Server::new(
        &tilo_api::Config {
            issuer: ISSUER.into(),
            web_entry: on_relay.web_entry.clone(),
            // Nothing serves at this address: no host is online for the key.
            hosts: vec!["http://127.0.0.1:9".into()],
        },
        tilo_api::store::Store::in_memory(),
    )
    .unwrap();
    let backend = V1 {
        api: &nowhere,
        token,
    };
    let (status, refused) = backend.get("/v1/workspaces").await;
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

/// What only the person asks of the server that keeps their workspace, each with what it is
/// called here: their vault listed and added to, a model key set, an agent named for their mail.
fn what_only_the_person_asks() -> [(&'static str, RemoteActionTarget, Value); 4] {
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
        (
            "names an agent for a mail account",
            person_accounts::target(person_accounts::ASSISTANT),
            json!({"account": "ada@example.test", "agent": "assistant", "may": true}),
        ),
    ]
}

/// What a person keeps for themselves alone where their workspace is kept (what they sign in
/// with, their model keys, which agent reads their mail) answers that person on a device of
/// theirs, and nobody else. A key they made for a script is certified as an agent's grant, so
/// its calls are their agent's: asked on the key's own session, as an API server asks for it,
/// each is told what a caller asking for something that does not exist is told.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_made_for_a_script_is_not_its_person_where_only_the_person_is_answered() {
    Box::pin(async {
        let served = api_on_relay(ISSUER).await;
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
        // The key as `tilo keys create` makes one: asked of the API, approved by Ada on her page.
        let token = approved_key(&served, &ada, &page, "Nightly build").await;
        let backend = V1 {
            api: &served.api,
            token: token.clone(),
        };
        let (status, made) = backend
            .post("/v1/workspaces", &json!({"name": "Ada's own"}), &[])
            .await;
        assert_eq!(status, 201, "{made}");
        let workspace = made["id"].as_str().expect("a workspace").to_owned();

        let client = tilo_net::Client::builder().build().unwrap();
        let holder = Arc::new(tilo_api::relay::Relay::new(&client, &served.relay));
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
        for (what, target, argument) in what_only_the_person_asks() {
            let answered = asked(&on_laptop.device, laptop_in, target, &argument).await;
            assert_ne!(
                answered.as_ref().err().map(String::as_str),
                Some("unknown_action"),
                "Ada, on a device she paired, {what}"
            );
        }
        let kept = asked(&on_laptop.device, laptop_in, list(), &json!({}))
            .await
            .expect("her own device lists her vault");
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
        for (what, target, argument) in what_only_the_person_asks() {
            let answered = asked(&with_key.device, key_in, target, &argument).await;
            assert_eq!(
                answered,
                Err("unknown_action".to_owned()),
                "a key made for a script {what}"
            );
        }

        // Nothing of hers moved: her vault holds what she kept, and nothing the key handed it.
        let after = asked(&on_laptop.device, laptop_in, list(), &json!({})).await;
        assert_eq!(after, Ok(kept));
    })
    .await;
}
