//! What a persistent workspace's deployment reaches from outside, beside the
//! browser listener its members use: the host's health at `GET /mode`.
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
use std::sync::Arc;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode, header};
use tilo_workspace_service::workspaces::mode::HostMode;

/// The image's source revision, set by its Containerfile from the same build
/// argument as the `org.opencontainers.image.revision` label.
const SOURCE_REVISION_ENV: &str = "TILO_SOURCE_REVISION";
/// What a build without a valid revision reports.
const UNKNOWN_REVISION: &str = "unknown";

/// A running listener. Dropping it stops serving.
pub(super) struct Handle {
    /// The address the listener bound: `--http 127.0.0.1:0` takes a port the
    /// kernel picks, which the workspace announces.
    pub addr: SocketAddr,
    routes: Arc<Routes>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Handle {
    /// The workspace serves: `/mode` answers `up`, and a browser that opens
    /// another address here goes to `web_entry`. A second call keeps the
    /// first.
    pub(super) fn serve(&self, web_entry: Option<String>) {
        let served = self.routes.serving.set(Http { web_entry });
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
        .with_context(|| format!("listen on {listen} for /mode"))?;
    let addr = listener
        .local_addr()
        .with_context(|| format!("the address of the listener on {listen}"))?;
    let routes = Arc::new(Routes {
        revision: source_revision(),
        serving: std::sync::OnceLock::new(),
        mode,
    });
    tracing::info!(%addr, "answering /mode");
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
    /// The web entry a browser that opened another address is sent to.
    web_entry: Option<String>,
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
                async move { Ok::<_, std::convert::Infallible>(handle(&http, request)) }
            });
            let _ =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
        });
    }
}

fn handle<B>(routes: &Routes, request: Request<B>) -> Response<Full<Bytes>> {
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
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["mode"] => json(&serde_json::json!({
            // The server serves: its workspaces are open and people get in.
            "server": "up",
            // The operator's kill switch: what the entry admits under right now.
            "mode": routes.mode.current().as_str(),
            "revision": routes.revision,
        })),
        // A browser that opened an address here asks for a page, not for a
        // text body: it goes to the site, which has one.
        _ if wants_a_page && http.web_entry.is_some() => {
            redirect(http.web_entry.as_deref().unwrap_or_default())
        }
        _ => error(StatusCode::NOT_FOUND, "not found"),
    }
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
        let response = handle(&http, probe());
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            format!(
                "{{\"server\":\"starting\",\"mode\":\"normal\",\"revision\":\"{UNKNOWN_REVISION}\"}}"
            )
        );
        assert!(http.serving.set(Http { web_entry: None }).is_ok());
        for method in [Method::POST, Method::PUT, Method::HEAD] {
            let request = Request::builder()
                .method(method.clone())
                .uri("/mode")
                .body(Full::new(Bytes::new()))
                .unwrap();
            assert_eq!(
                handle(&http, request).status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method}"
            );
        }
        let request = Request::builder()
            .uri("/mode")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = handle(&http, request);
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
            let response = handle(&http, up());
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
                handle(&http, request).status(),
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        drop(http);
    }
}
