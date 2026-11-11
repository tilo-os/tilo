//! What a persistent workspace's deployment reaches from outside, beside the
//! browser listener its members use: the host's health at `GET /mode`, the
//! avatars its bots publish at `GET /avatars/<name>`, and Discord's sign-in
//! callback at `GET /discord/callback`.
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
use tilo_relay_protocol::public_avatars::{MAX_AVATAR_BYTES, is_avatar_name, sniff};

use super::bots::hand_to_discord;
use super::discord_sign_in::{self, Failed, SignIn};

/// The image's source revision, set by its Containerfile from the same build
/// argument as the `org.opencontainers.image.revision` label.
const SOURCE_REVISION_ENV: &str = "TILO_SOURCE_REVISION";
/// What a build without a valid revision reports.
const UNKNOWN_REVISION: &str = "unknown";

pub(super) struct Config {
    pub listen: SocketAddr,
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
    /// kernel picks, which the room announces.
    pub addr: SocketAddr,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Serve `config` until the returned [`Handle`] is dropped.
///
/// # Errors
/// The address cannot be bound.
pub(super) async fn serve(config: Config) -> anyhow::Result<Handle> {
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| {
            format!(
                "listen on {} for /mode, /avatars and /discord/callback",
                config.listen
            )
        })?;
    let addr = listener
        .local_addr()
        .with_context(|| format!("the address of the listener on {}", config.listen))?;
    let http = Arc::new(Http {
        revision: source_revision(),
        avatars: config.avatars,
        sign_in: config.sign_in,
        web_entry: config.web_entry,
        discord: config.discord,
    });
    tracing::info!(%addr, "serving /mode, /avatars and /discord/callback");
    Ok(Handle {
        addr,
        tasks: vec![tokio::spawn(accept_loop(listener, http))],
    })
}

struct Http {
    /// The commit this binary was built from, or [`UNKNOWN_REVISION`].
    revision: String,
    avatars: Option<PathBuf>,
    sign_in: Option<SignIn>,
    web_entry: Option<String>,
    discord: Option<Arc<ConnectorProcess>>,
}

async fn accept_loop(tcp: tokio::net::TcpListener, http: Arc<Http>) {
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

async fn handle<B>(http: &Http, request: Request<B>) -> Response<Full<Bytes>> {
    if request.method() != Method::GET {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["mode"] => json(&serde_json::json!({
            // Whether the relay serves: this listener runs in its process, so
            // it answers at all only while the relay started and is serving.
            "relay": "up",
            "revision": http.revision,
        })),
        ["avatars", name] => avatar(http, name).await,
        ["discord", "callback"] => http.callback(&query).await,
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

/// A sign-in with Discord that linked nothing: a plain page saying why and
/// what to do, with the way back into Tilo.
fn failed_page(said: &str, web_entry: &str) -> Response<Full<Bytes>> {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let page = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\">\
         <title>Linking Discord didn't work</title>\
         <body style=\"font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 1.5rem;color:#1c1c1e\">\
         <h1 style=\"font-size:1.4rem\">Linking Discord didn't work</h1><p>{}</p>\
         <p><a href=\"{}\" style=\"color:#5865f2\">Back to Tilo</a></p>",
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
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use http_body_util::BodyExt as _;

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
    async fn only_get_is_answered() {
        let http = Http {
            revision: UNKNOWN_REVISION.to_owned(),
            avatars: None,
            sign_in: None,
            web_entry: None,
            discord: None,
        };
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
            format!("{{\"relay\":\"up\",\"revision\":\"{UNKNOWN_REVISION}\"}}")
        );
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
    }
}
