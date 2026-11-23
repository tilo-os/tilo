//! What a persistent workspace's deployment reaches from outside, beside the
//! browser listener its members use: the host's health at `GET /mode`, the
//! avatars its bots publish at `GET /avatars/<name>`, and Discord's sign-in
//! callback at `GET /discord/callback`.
//!
//! The listener answers from the moment the process starts: `/mode` says
//! `starting` (503) while the host opens its workspaces, and `up` (200) once
//! people can get in, so a deployment tells a host that is loading from one
//! that is gone. It also says the operator's kill switch: `normal`,
//! `spectator-only`, or `back-soon`, from the `mode` file in the state
//! directory, which the workspace's entry reads so a paused host admits nobody new without a
//! restart.
//!
//! Plain HTTP: the ingress in front of it holds the certificate and the
//! public address, and the health check reads the same address a person's
//! browser was sent to.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode, header};
use tilo_connector::ConnectorProcess;
use tilo_server_protocol::public_avatars::{MAX_AVATAR_BYTES, is_avatar_name, sniff};
use tilo_workspace_service::workspaces::mode::HostMode;

use super::bots::hand_to_discord;
use super::discord_sign_in::{self, Failed, SignIn};

/// The image's source revision, set by its Containerfile from the same build
/// argument as the `org.opencontainers.image.revision` label.
const SOURCE_REVISION_ENV: &str = "TILO_SOURCE_REVISION";
/// What a build without a valid revision reports.
const UNKNOWN_REVISION: &str = "unknown";

pub(super) struct Serving {
    /// Where the avatars bots publish are, when the workspace publishes any.
    pub avatars: Option<PathBuf>,
    /// Signing in with Discord, when the workspace offers it.
    pub sign_in: Option<SignIn>,
    /// The web entry the Discord callback sends the browser back to; without
    /// one there is nowhere to send it and the route answers 404.
    pub web_entry: Option<String>,
    /// The workspace's Discord bridge, which a linked account is handed to.
    pub discord: Option<Arc<ConnectorProcess>>,
}

/// A running listener. Dropping it stops serving.
pub(super) struct Handle {
    /// The address the listener bound: `--http 127.0.0.1:0` takes a port the
    /// kernel picks, which the workspace announces.
    pub addr: SocketAddr,
    routes: Arc<Routes>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Handle {
    /// The workspace serves: `/mode` answers `up`, and the other routes
    /// answer from `serving`. A second call keeps the first.
    pub(super) fn serve(&self, serving: Serving) {
        let served = self.routes.serving.set(Http {
            avatars: serving.avatars,
            sign_in: serving.sign_in,
            web_entry: serving.web_entry,
            discord: serving.discord,
        });
        if served.is_err() {
            tracing::warn!("the deployment routes already serve; they keep what they serve");
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Listen on `listen` until the returned [`Handle`] is dropped: `/mode`
/// says the host is starting until [`Handle::serve`], and names the operator's kill
/// switch from the mode `mode` reads.
///
/// # Errors
/// The address cannot be bound.
pub(super) async fn listen(listen: SocketAddr, mode: Arc<HostMode>) -> anyhow::Result<Handle> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("listen on {listen} for /mode, /avatars and /discord/callback"))?;
    let addr = listener
        .local_addr()
        .with_context(|| format!("the address of the listener on {listen}"))?;
    let routes = Arc::new(Routes {
        revision: source_revision(),
        serving: std::sync::OnceLock::new(),
        mode,
    });
    tracing::info!(%addr, "answering /mode, /avatars and /discord/callback");
    Ok(Handle {
        addr,
        tasks: vec![tokio::spawn(accept_loop(listener, Arc::clone(&routes)))],
        routes,
    })
}

struct Routes {
    /// The commit this binary was built from, or [`UNKNOWN_REVISION`].
    revision: String,
    /// What the routes answer from, once the workspace serves.
    serving: std::sync::OnceLock<Http>,
    /// The operator's kill switch, as `/mode` reports it now.
    mode: Arc<HostMode>,
}

struct Http {
    avatars: Option<PathBuf>,
    sign_in: Option<SignIn>,
    web_entry: Option<String>,
    discord: Option<Arc<ConnectorProcess>>,
}

async fn accept_loop(tcp: tokio::net::TcpListener, http: Arc<Routes>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, _)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        let http = Arc::clone(&http);
        connections.spawn(async move {
            let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                let http = Arc::clone(&http);
                async move { Ok::<_, std::convert::Infallible>(handle(&http, request).await) }
            });
            let _ =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
        });
    }
}

async fn handle<B>(routes: &Routes, request: Request<B>) -> Response<Full<Bytes>> {
    if request.method() != Method::GET {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }
    let Some(http) = routes.serving.get() else {
        // Still opening its workspaces: not ready for people, but alive.
        let mut starting = json(&serde_json::json!({
            "server": "starting",
            "mode": routes.mode.current().as_str(),
            "revision": routes.revision,
        }));
        *starting.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        return starting;
    };
    let wants_a_page = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|accept| accept.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["mode"] => json(&serde_json::json!({
            // The server serves: its workspaces are open and people get in.
            "server": "up",
            // The operator's kill switch: what the entry admits under right now.
            "mode": routes.mode.current().as_str(),
            "revision": routes.revision,
        })),
        ["avatars", name] => avatar(http, name).await,
        ["discord", "callback"] => http.callback(&query).await,
        // A browser that opened an address here asks for a page, not for a
        // text body: it goes to the site, which has one.
        _ if wants_a_page && http.web_entry.is_some() => {
            redirect(http.web_entry.as_deref().unwrap_or_default())
        }
        _ => error(StatusCode::NOT_FOUND, "not found"),
    }
}

/// `GET /avatars/<name>`: an avatar a bot published, by its content address.
async fn avatar(http: &Http, name: &str) -> Response<Full<Bytes>> {
    let Some(dir) = &http.avatars else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    if !is_avatar_name(name) {
        return error(StatusCode::NOT_FOUND, "not found");
    }
    let Ok(bytes) = tokio::fs::read(dir.join(name)).await else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let Some(kind) = sniff(&bytes).filter(|_| bytes.len() <= MAX_AVATAR_BYTES) else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let mut response = Response::new(Full::new(Bytes::from(bytes)));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static(kind));
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

impl Http {
    /// `GET /discord/callback`: back from Discord's consent page, on to the web
    /// entry, or a page saying what went wrong.
    async fn callback(&self, query: &str) -> Response<Full<Bytes>> {
        let (Some(sign_in), Some(entry)) = (&self.sign_in, &self.web_entry) else {
            return error(StatusCode::NOT_FOUND, "not found");
        };
        match sign_in.callback(query).await {
            // Handed over before the browser is sent on: the page it lands on
            // asks the workspace for the offer at once, so a sign-in the
            // bridge never got is not one the person is told worked.
            Ok(authorized) if hand_to_discord(self.discord.as_ref(), &authorized).await => {
                redirect(&discord_sign_in::back_to_tilo(
                    entry,
                    Some(&authorized.token),
                ))
            }
            Ok(_) => failed_page(Failed::Discord.said(), entry),
            Err(Failed::Declined) => redirect(&discord_sign_in::back_to_tilo(entry, None)),
            Err(failed) => failed_page(failed.said(), entry),
        }
    }
}

/// A sign-in with Discord that linked nothing: the product's own card saying
/// why, with the one way on — back to Tilo, where Connect Discord starts it
/// again.
fn failed_page(said: &str, web_entry: &str) -> Response<Full<Bytes>> {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let page = format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>Discord didn't link</title>\
<style>\
:root{{color-scheme:dark;--ground:#121214;--card:#1c1d21;--line:#2c2e34;--shadow:rgb(0 0 0/45%);--ink:#ddd;--heading:#f2f2f4;--text:#b9b9c0;--primary-bg:#f2f2f4;--primary-ink:#121214;--primary-hover:#fff}}\
@media (prefers-color-scheme:light){{:root{{color-scheme:light;--ground:#f5f5f7;--card:#fff;--line:#e2e3e8;--shadow:rgb(15 18 32/12%);--ink:#26272b;--heading:#121214;--text:#55565e;--primary-bg:#1b1c20;--primary-ink:#fff;--primary-hover:#000}}}}\
html,body{{margin:0;min-height:100%;background:var(--ground);color:var(--ink);font:14px/1.5 system-ui,sans-serif}}\
body{{display:grid;place-items:center;box-sizing:border-box;padding:16px}}\
.card{{display:grid;gap:18px;box-sizing:border-box;width:min(380px,100%);padding:26px;border-radius:14px;background:var(--card);border:1px solid var(--line);box-shadow:0 12px 40px var(--shadow)}}\
.card svg{{display:block}}\
h1{{margin:0;font-size:19px;font-weight:600;line-height:1.35;color:var(--heading);text-wrap:balance}}\
p{{margin:0;color:var(--text)}}\
a.primary{{justify-self:start;display:inline-block;padding:8px 14px;border-radius:8px;background:var(--primary-bg);color:var(--primary-ink);font-weight:500;text-decoration:none}}\
a.primary:hover{{background:var(--primary-hover)}}\
a.primary:focus-visible{{outline:2px solid #9ab4ff;outline-offset:2px}}\
</style></head>\
<body><main class=card aria-labelledby=heading>\
<svg aria-hidden=true viewBox=\"0 0 100 100\" width=40 height=40>\
<path d=\"M10.98 31.85 Q10.979892448752274 27.96648782251284 14.4 26.12 L20.26 22.97 Q24.843373493975903 20.50602409638554 24.84 25.71 L24.84 74.29 Q24.843373493975903 79.49397590361446 20.26 77.03 L14.4 73.88 Q10.979892448752274 72.03351217748715 10.98 68.15 Z\" fill=\"#3b4376\"/>\
<path d=\"M89.02 31.85 Q89.02010755124772 27.96648782251284 85.6 26.12 L79.74 22.97 Q75.1566265060241 20.50602409638554 75.16 25.71 L75.16 74.29 Q75.1566265060241 79.49397590361446 79.74 77.03 L85.6 73.88 Q89.02010755124772 72.03351217748715 89.02 68.15 Z\" fill=\"#3b4376\"/>\
<rect x=29 y=16 width=42 height=68 rx=6 fill=\"#6f82ff\"/></svg>\
<h1 id=heading>Discord didn't link</h1><p>{}</p>\
<a class=primary href=\"{}\">Back to Tilo</a></main></body></html>",
        escape(said),
        escape(web_entry)
    );
    let mut response = Response::new(Full::new(Bytes::from(page)));
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

fn redirect(to: &str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::FOUND;
    if let Ok(value) = header::HeaderValue::from_str(to) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

fn json(value: &impl serde::Serialize) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

fn error(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::copy_from_slice(message.as_bytes())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// A full commit id, or [`UNKNOWN_REVISION`]: the value becomes a JSON field
/// a deploy compares with the digest it pins, so nothing else from the
/// environment passes.
fn source_revision() -> String {
    parse_revision(std::env::var(SOURCE_REVISION_ENV).ok().as_deref())
}

fn parse_revision(value: Option<&str>) -> String {
    match value {
        Some(revision)
            if revision.len() == 40
                && revision
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            revision.to_owned()
        }
        _ => UNKNOWN_REVISION.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt as _;
    use tilo_workspace_service::workspaces::mode::Mode;

    use super::*;

    #[test]
    fn the_health_page_reports_a_full_commit_and_nothing_else_from_the_environment() {
        assert_eq!(
            parse_revision(Some("0123456789abcdef0123456789abcdef01234567")),
            "0123456789abcdef0123456789abcdef01234567"
        );
        for other in [
            None,
            Some("4df7ab12"),
            Some("0123456789ABCDEF0123456789ABCDEF01234567"),
            Some("0123456789abcdef0123456789abcdef0123456g"),
            Some("0123456789abcdef0123456789abcdef012345678"),
        ] {
            assert_eq!(parse_revision(other), UNKNOWN_REVISION, "{other:?}");
        }
    }

    #[tokio::test]
    async fn a_link_that_linked_nothing_says_why_and_the_way_back_is_escaped() {
        let response = failed_page("a \"quote\" & <b>", "https://tilo.example/?a=1&b=2");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("a &quot;quote&quot; &amp; &lt;b&gt;"),
            "{body}"
        );
        assert!(
            body.contains("href=\"https://tilo.example/?a=1&amp;b=2\""),
            "{body}"
        );
    }

    #[tokio::test]
    async fn mode_says_starting_until_the_workspace_serves_and_only_get_is_answered() {
        let state = tempfile::tempdir().unwrap();
        let held = HostMode::held(state.path().to_owned(), Mode::Normal);
        let http = Routes {
            revision: UNKNOWN_REVISION.to_owned(),
            serving: std::sync::OnceLock::new(),
            mode: Arc::clone(&held),
        };
        let probe = || {
            Request::builder()
                .uri("/mode")
                .body(Full::new(Bytes::new()))
                .unwrap()
        };
        let response = handle(&http, probe()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            format!(
                "{{\"server\":\"starting\",\"mode\":\"normal\",\"revision\":\"{UNKNOWN_REVISION}\"}}"
            )
        );
        assert!(
            http.serving
                .set(Http {
                    avatars: None,
                    sign_in: None,
                    web_entry: None,
                    discord: None,
                })
                .is_ok()
        );
        for method in [Method::POST, Method::PUT, Method::HEAD] {
            let request = Request::builder()
                .method(method.clone())
                .uri("/mode")
                .body(Full::new(Bytes::new()))
                .unwrap();
            assert_eq!(
                handle(&http, request).await.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method}"
            );
        }
        let request = Request::builder()
            .uri("/mode")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = handle(&http, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            format!(
                "{{\"server\":\"up\",\"mode\":\"normal\",\"revision\":\"{UNKNOWN_REVISION}\"}}"
            )
        );

        // The operator turns the kill switch by writing the mode file: `/mode` says which of the
        // three names is in force, so a deployment that paused the host can see it took.
        let up = || {
            Request::builder()
                .uri("/mode")
                .body(Full::new(Bytes::new()))
                .unwrap()
        };
        for (written, said) in [
            ("spectator-only", "spectator-only"),
            ("back-soon", "back-soon"),
            ("normal", "normal"),
        ] {
            std::fs::write(state.path().join("mode"), written).unwrap();
            let response = handle(&http, up()).await;
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let body = String::from_utf8(body.to_vec()).unwrap();
            assert!(
                body.contains(&format!("\"mode\":\"{said}\"")),
                "{written}: {body}"
            );
        }
        assert_eq!(held.current(), Mode::Normal);
        for path in [
            "/",
            "/wait-list",
            "/avatars/not-a-name",
            "/discord/callback",
        ] {
            let request = Request::builder()
                .uri(path)
                .body(Full::new(Bytes::new()))
                .unwrap();
            assert_eq!(
                handle(&http, request).await.status(),
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        drop(http);
    }
}
