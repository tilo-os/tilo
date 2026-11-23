//! The route a link to a workspace's files is fetched at
//! ([`crate::content_links`]): by the browser's download manager when a file
//! is dragged out of a page, and by a page streaming a file it saves.

use bytes::Bytes;
use hyper::{Method, Response, StatusCode, header};

use super::{Body, Web, reply, status};
use crate::content_links::{self, Sink};

/// How many buffers wait to be sent before reading the store waits for the
/// browser: what a link holds in memory, whatever the file's size.
const QUEUED: usize = 4;

/// The bytes a link's reading thread hands the response.
struct Channel(tokio::sync::mpsc::Sender<Result<Bytes, String>>);

impl Sink for Channel {
    fn send(&mut self, bytes: Vec<u8>) -> bool {
        self.0.blocking_send(Ok(Bytes::from(bytes))).is_ok()
    }
}

/// Answer one request for the link `link` (`<token>/<name>`). `view` asks for
/// the file shown in the browser's tab rather than saved: it is, where the
/// file is one a browser plays or shows safely, and saved otherwise.
pub(super) async fn handle(web: &Web, method: &Method, link: &str, view: bool) -> Response<Body> {
    if method == Method::OPTIONS {
        return reply(StatusCode::NO_CONTENT, Vec::new());
    }
    if method != Method::GET && method != Method::HEAD {
        return status(StatusCode::METHOD_NOT_ALLOWED, "GET only");
    }
    let Some(grant) = web.content_links.grant(link) else {
        return status(
            StatusCode::NOT_FOUND,
            "this link has expired; drag or save the file again",
        );
    };
    let store = grant.store.clone();
    let ask = grant.ask.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let streaming = content_links::prepare(&store, &ask);
        (store, ask, streaming)
    })
    .await;
    let Ok((store, ask, streaming)) = prepared else {
        return status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the files could not be read",
        );
    };
    let streaming = match streaming {
        Ok(streaming) => streaming,
        Err(refusal @ content_links::Refusal::Missing(_)) => {
            return status(StatusCode::SERVICE_UNAVAILABLE, &refusal.message());
        }
        Err(refusal @ content_links::Refusal::TooLarge) => {
            return status(StatusCode::PAYLOAD_TOO_LARGE, &refusal.message());
        }
    };
    let length = streaming.length;
    let body = if method == Method::HEAD {
        Body::Full(None)
    } else {
        let (sender, receiver) = tokio::sync::mpsc::channel(QUEUED);
        tokio::task::spawn_blocking(move || {
            let failed = sender.clone();
            if let Err(why) = content_links::stream(&store, &streaming, &mut Channel(sender)) {
                tracing::warn!(%why, "a linked file broke off");
                let _ = failed.blocking_send(Err(why));
            }
        });
        Body::Streamed(receiver)
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    let shown = (view && !ask.zip)
        .then(|| tilo_actions::remote::viewable(&ask.name))
        .flatten();
    let kind = match shown {
        Some(kind) => kind,
        None if ask.zip => "application/zip",
        None => "application/octet-stream",
    };
    headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static(kind));
    headers.insert(header::CONTENT_LENGTH, header::HeaderValue::from(length));
    let disposition = if shown.is_some() {
        content_links::inline_disposition(&ask.name)
    } else {
        content_links::disposition(&ask.name)
    };
    if let Ok(disposition) = header::HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, disposition);
    }
    if shown.is_some() {
        // Shown on this origin, so nothing it holds may act there: no
        // script, no form, and the type it was named, never one guessed.
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            header::HeaderValue::from_static(
                "sandbox; default-src 'none'; img-src 'self'; media-src 'self'",
            ),
        );
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            header::HeaderValue::from_static("nosniff"),
        );
    }
    // A link names its bytes for good: what it serves never changes, and no
    // cache should keep a capability's answer.
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    response
}
