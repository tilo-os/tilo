//! The answer `serve` gives for a peer request: a file on this machine,
//! read where it is.
//!
//! A page's Files app browses the workspace host's disk the way the desktop
//! session browses its own, so the host answers a `GET` of a path with the
//! path's bytes — the same request, on either machine, over one session.
//!
//! What it answers is a read of one file: its bytes, and an `ETag` from
//! them, so a reader that asks again for what it already has gets a `304`
//! and no transfer. The listing, the search and the metadata a Files app
//! shows before it reads any bytes are `fs`'s, not this module's.

use std::path::Path;

use ::http::{Method, Response, StatusCode, header};
use bytes::Bytes;

use super::ProxyFailure;

/// The most of one file a peer request relays: the same bound as any other
/// response ([`super::Config::max_response_bytes`]'s default), so a reader
/// of a large file asks again rather than the proxy holding it whole.
const LARGEST: u64 = 64 * 1024 * 1024;

/// An empty answer is always this: a status and static headers.
const TEXT: &str = "text/plain; charset=utf-8";

/// The answer `method` gets of `path` on this machine: the file's bytes for
/// a `GET`, its headers for a `HEAD`, and the proxy's own refusal for
/// anything else — a page reads a file, it does not write one.
pub(super) fn answer(path: &Path, method: &Method) -> reqwest::Response {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return refusal(ProxyFailure::BadRequest);
    }
    read_answer(path, *method == Method::HEAD)
}

/// The answer to a `GET` (or, with `head`, a `HEAD`) of `path` on this
/// machine.
fn read_answer(path: &Path, head: bool) -> reqwest::Response {
    let content_type = content_type_of(path);
    if head {
        return empty(StatusCode::OK, content_type);
    }
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() as u64 <= LARGEST => content(bytes, content_type),
        Ok(_) => empty(StatusCode::PAYLOAD_TOO_LARGE, TEXT),
        Err(error) => empty(failure(&error), TEXT),
    }
}

/// What a file's own error is in HTTP's terms: a file that is not there, or
/// one this process may not read.
fn failure(error: &std::io::Error) -> StatusCode {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => StatusCode::NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        std::io::ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// A `200` whose body is `bytes`, its `ETag` from them.
fn content(bytes: Vec<u8>, content_type: &'static str) -> reqwest::Response {
    let etag = format!("\"{:016x}\"", checksum(&bytes));
    let length = bytes.len();
    let built = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, length)
        .header(header::ETAG, etag)
        .body(reqwest::Body::from(Bytes::from(bytes)));
    match built {
        Ok(answer) => answer.into(),
        Err(_) => empty(StatusCode::INTERNAL_SERVER_ERROR, TEXT),
    }
}

/// An answer with `status`, static headers and no body.
fn empty(status: StatusCode, content_type: &'static str) -> reqwest::Response {
    let built = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_LENGTH, 0)
        .body(reqwest::Body::from(Bytes::new()));
    match built {
        Ok(answer) => answer.into(),
        // A status and static headers are a response; when they are not, the
        // only honest answer left is that this machine could not read the
        // file.
        Err(_) => Response::new(reqwest::Body::from(Bytes::new())).into(),
    }
}

/// A file's type from its name, so a reader knows what it has before it
/// reads it.
fn content_type_of(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "txt" | "log" | "toml" | "rs" | "py" | "sh" | "c" | "h" | "css" | "csv" => TEXT,
        "json" => "application/json",
        "html" | "htm" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

/// A stable fingerprint of `bytes`: a file's identity by content, for the
/// `ETag` a reader compares.
fn checksum(bytes: &[u8]) -> u64 {
    // FNV-1a: cheap, and telling one revision from another is all an `ETag`
    // asks of it.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The proxy's answer to a peer request it will not carry out at all, as
/// another request's refusal is (§[`super::failure_response`]).
pub(super) fn refusal(failure: ProxyFailure) -> reqwest::Response {
    let built = Response::builder()
        .status(StatusCode::from_u16(failure.status()).unwrap_or(StatusCode::BAD_REQUEST))
        .header(super::ERROR_HEADER, failure.code())
        .header(header::CONTENT_TYPE, TEXT)
        .header(header::CACHE_CONTROL, "no-store");
    match built.body(reqwest::Body::from(Bytes::new())) {
        Ok(answer) => answer.into(),
        Err(_) => empty(StatusCode::BAD_REQUEST, TEXT),
    }
}
