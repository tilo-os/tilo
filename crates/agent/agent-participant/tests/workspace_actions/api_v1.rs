//! The API's `/v1` routes against a relay host, as tilo.sh runs them: a
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
#[expect(
    clippy::too_many_lines,
    reason = "one backend's story through the API, from its first workspace to deleting it"
)]
async fn a_key_works_through_the_api_on_a_relay_host() {
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
            &json!({"url": url, "events": ["run"]}),
            &[],
        )
        .await;
    assert_eq!(status, 201, "{hook}");
    let secret = hook["secret"].as_str().unwrap().to_owned();

    let run_headers = [
        ("tilo-run", "fill-jane"),
        ("tilo-run-title", "Fill Jane's application"),
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
    let run = imported["run"].as_str().unwrap().to_owned();
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
    assert_eq!(retried["run"], json!(run), "{retried}");
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
    let (status, followed) = backend.get(&format!("{base}/runs/{run}")).await;
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

    // Deleting the workspace revokes the key that reached only it.
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
