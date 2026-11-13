//! File bytes into a workspace, answering the `tilo.content:<hash>` reference
//! that the workspace's actions take a file by.
//!
//! It is the runtime's own upload (`tilo_put_begin`, `tilo_put_append`,
//! `tilo_put_finish`), carried in chunks by the server so a program need not
//! base64 a passport scan into MCP messages, in the key's API run.
//!
//! A file goes up in one request (`POST /v1/workspaces/{workspace}/uploads`
//! with its bytes) or, when it is larger than one request should carry, in as
//! many as it takes: `POST …/uploads/{upload}` opens an upload of its size
//! under an id the caller chose, `PUT …/uploads/{upload}` carries the next
//! bytes at the offset the upload stands at — read as they arrive, never held
//! whole — and the piece that completes it answers the content. An upload a
//! lost connection cut stays open: the next `PUT` continues it, and `GET
//! …/uploads/{upload}` says where it stands.

use crate::lock;
use crate::session::Session;
use crate::v1::errors::ApiError;
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The most one request's body may carry: a whole small file, or one piece
/// of a larger one.
pub const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
/// The most raw bytes one `tilo_put_append` carries.
const CHUNK: usize = 512 * 1024;
/// The most of an incoming piece the server holds before sending it on.
pub(crate) const PIECE: usize = CHUNK;
/// How long an upload nothing is sent to stays open.
const OPEN_FOR: Duration = Duration::from_secs(6 * 60 * 60);
/// How many uploads one server keeps open at once; past it, the one left
/// untouched longest is dropped, storing nothing.
const MOST_OPEN: usize = 64;

/// The refusal of a body past [`MAX_CONTENT_BYTES`].
#[must_use]
pub fn too_large() -> ApiError {
    ApiError::new("too_large", "one request carries at most 64 MiB")
        .next("open an upload with its size and send it in pieces")
}

/// The refusal of a piece that would run past the upload's size.
fn too_much(end: u64, size: u64) -> ApiError {
    ApiError::new(
        "too_much",
        format!("this piece runs to {end} of the {size} bytes the upload began with"),
    )
    .next("send no more than the upload's size")
}

/// The refusal of a piece the workspace would not take.
fn piece_refused(why: &str) -> ApiError {
    ApiError::new("upload_failed", why)
        .next("send the rest from where the upload stands, or open it again")
}

/// The upload no key of that workspace has open under `id`.
fn unknown() -> ApiError {
    ApiError::new("not_found", "no upload of this key is open under that id")
        .next("open one with POST /v1/workspaces/{workspace}/uploads/{upload} {\"size\": n}")
}

/// Whether `id` may name an upload: letters, digits, `-` and `_`, at a
/// length a key can be told and no other key guesses.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// The answer `field` of one of the upload tools' results.
fn answer(result: &Value, field: &str) -> Result<Value, String> {
    let text = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(text.to_owned());
    }
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| value.get(field).cloned())
        .or_else(|| {
            result
                .pointer(&format!("/structuredContent/{field}"))
                .cloned()
        })
        .ok_or_else(|| format!("the workspace answered no {field}: {text}"))
}

/// One file's bytes through `session` into `workspace` at once: its content
/// reference.
pub(crate) async fn put(session: &Session, workspace: &str, body: &[u8]) -> Result<Value, String> {
    let begun = session
        .call_in(workspace, "tilo_put_begin", json!({"size": body.len()}))
        .await?;
    let upload = answer(&begun, "upload")?;
    let written = send(session, workspace, &upload, 0, body).await?;
    if written != body.len() as u64 {
        return Err(format!(
            "the workspace took {written} of the {} bytes",
            body.len()
        ));
    }
    let finished = session
        .call_in(workspace, "tilo_put_finish", json!({"upload": upload}))
        .await?;
    answer(&finished, "content")
}

/// Send `chunk` at `offset` to `upload` in `workspace`, in the pieces one
/// `tilo_put_append` carries; answers the bytes the workspace accepted.
async fn send(
    session: &Session,
    workspace: &str,
    upload: &Value,
    offset: u64,
    chunk: &[u8],
) -> Result<u64, String> {
    let mut written = offset;
    for piece in chunk.chunks(CHUNK) {
        let appended = session
            .call_in(
                workspace,
                "tilo_put_append",
                json!({
                    "upload": upload,
                    "offset": written,
                    "chunk": base64::engine::general_purpose::STANDARD.encode(piece),
                }),
            )
            .await?;
        let accepted = answer(&appended, "written")?;
        written = accepted
            .as_u64()
            .unwrap_or_else(|| written.saturating_add(piece.len() as u64));
    }
    Ok(written)
}

/// One upload the server keeps open on its caller's behalf: the workspace's
/// own upload, where it stands, and who may send the rest.
struct Open {
    grant: String,
    workspace: String,
    /// The runtime's id for the upload, as `tilo_put_begin` answered it.
    upload: Value,
    /// What the upload began with.
    size: u64,
    /// Bytes the workspace accepted so far.
    written: u64,
    touched: Instant,
}

impl Open {
    /// Where the upload stands, as every answer but the last says it.
    fn placement(&self, id: &str) -> Value {
        json!({"upload": id, "size": self.size, "offset": self.written})
    }
}

/// What a piece of an upload was answered: where it stands, or the content
/// its last piece completed.
pub(crate) enum Sent {
    More(Value),
    Done(Value),
}

/// The uploads the server keeps open, by the id its caller named each.
#[derive(Default)]
pub struct Uploads {
    open: Mutex<HashMap<String, Arc<Mutex<Open>>>>,
}

impl Uploads {
    /// Open an upload of `size` bytes in `workspace` as `grant`'s under
    /// `id`, through `session`: what to send it by, and whether it is new.
    /// An upload the same key opened under `id` answers where it stands, so a
    /// client that lost the answer resumes instead of starting again.
    pub(crate) async fn open(
        &self,
        session: &Session,
        grant: &str,
        workspace: &str,
        id: &str,
        size: u64,
    ) -> Result<(Value, bool), ApiError> {
        let named = lock(&self.open).get(id).cloned();
        if let Some(open) = named {
            let mut held = lock(&open);
            if held.grant != grant || held.workspace != workspace {
                return Err(unknown());
            }
            held.touched = Instant::now();
            return Ok((held.placement(id), false));
        }
        let busy = |why: String| ApiError::new("unavailable", why).next("open the upload again");
        let begun = session
            .call_in(workspace, "tilo_put_begin", json!({"size": size}))
            .await
            .map_err(busy)?;
        let upload = answer(&begun, "upload").map_err(busy)?;
        let open = Open {
            grant: grant.to_owned(),
            workspace: workspace.to_owned(),
            upload,
            size,
            written: 0,
            touched: Instant::now(),
        };
        {
            let mut all = lock(&self.open);
            all.retain(|_, open| lock(open).touched.elapsed() < OPEN_FOR);
            while all.len() >= MOST_OPEN {
                let Some(stalest) = all
                    .iter()
                    .min_by_key(|(_, open)| lock(open).touched)
                    .map(|(id, _)| id.clone())
                else {
                    break;
                };
                all.remove(&stalest);
            }
            all.insert(id.to_owned(), Arc::new(Mutex::new(open)));
        }
        Ok((json!({"upload": id, "size": size, "offset": 0}), true))
    }

    /// The open upload `id` of `grant`'s in `workspace`, touched, or the
    /// refusal of a request that names none.
    fn mine(&self, grant: &str, workspace: &str, id: &str) -> Result<Arc<Mutex<Open>>, ApiError> {
        let Some(open) = lock(&self.open).get(id).cloned() else {
            return Err(unknown());
        };
        let mut held = lock(&open);
        if held.grant != grant || held.workspace != workspace {
            return Err(unknown());
        }
        // Every byte of it is there: its last piece answered the content and
        // it is gone, so nothing more goes to it.
        if held.written >= held.size {
            return Err(ApiError::new(
                "upload_complete",
                "every byte of that upload is there: its last piece answered its content",
            )
            .next("use the content reference that answer named"));
        }
        held.touched = Instant::now();
        drop(held);
        Ok(open)
    }

    /// Where the open upload `id` of `grant`'s stands.
    pub fn status(&self, grant: &str, workspace: &str, id: &str) -> Result<Value, ApiError> {
        let open = self.mine(grant, workspace, id)?;
        Ok(lock(&open).placement(id))
    }

    /// Drop the open upload `id` of `grant`'s: its bytes are never stored.
    pub fn abandon(&self, grant: &str, workspace: &str, id: &str) -> Result<Value, ApiError> {
        let mine = lock(&self.open).get(id).is_some_and(|open| {
            let open = lock(open);
            open.grant == grant && open.workspace == workspace
        });
        if !mine {
            return Err(unknown());
        }
        lock(&self.open).remove(id);
        Ok(json!({"deleted": id}))
    }

    /// Send `chunk` to the open upload `id` of `grant`'s, answering where it
    /// stands — or its content, when this piece completes it.
    ///
    /// `at` is where the caller believes the upload stands. It must be where
    /// it does: a piece sent again after a lost answer is refused rather than
    /// stored twice, and the refusal names the offset to send from.
    pub(crate) async fn append(
        &self,
        session: &Session,
        grant: &str,
        workspace: &str,
        id: &str,
        at: Option<u64>,
        chunk: &[u8],
    ) -> Result<Sent, ApiError> {
        let open = self.mine(grant, workspace, id)?;
        let (upload, offset, size) = {
            let open = lock(&open);
            (open.upload.clone(), open.written, open.size)
        };
        if let Some(at) = at
            && at != offset
        {
            return Err(ApiError::new(
                "offset_mismatch",
                format!("the upload has {offset} of its {size} bytes, not {at}"),
            )
            .next("send the next piece from the offset the upload stands at"));
        }
        let end = offset.saturating_add(chunk.len() as u64);
        if end > size {
            return Err(too_much(end, size));
        }
        let written = send(session, workspace, &upload, offset, chunk)
            .await
            .map_err(|why| {
                // The workspace refused the bytes: the upload is over.
                lock(&self.open).remove(id);
                piece_refused(&why)
            })?;
        let placement = {
            let mut held = lock(&open);
            held.written = written;
            held.touched = Instant::now();
            held.placement(id)
        };
        if written < size {
            return Ok(Sent::More(placement));
        }
        let finished = session
            .call_in(workspace, "tilo_put_finish", json!({"upload": upload}))
            .await
            .and_then(|finished| answer(&finished, "content"))
            .map_err(|why| {
                lock(&self.open).remove(id);
                piece_refused(&why)
            });
        lock(&self.open).remove(id);
        Ok(Sent::Done(json!(
            {"content": finished?, "size": size}
        )))
    }
}

/// Whether `method` on `path` is an upload's next piece, which the server
/// reads as it arrives rather than holding it whole.
#[must_use]
pub fn streams(method: &hyper::Method, path: &str) -> bool {
    *method == hyper::Method::PUT
        && path
            .trim_end_matches('/')
            .rsplit('/')
            .nth(1)
            .is_some_and(|segment| segment == "uploads")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    /// An upload's own id names it; anything a caller could not have been
    /// told, or that a path would confuse, does not.
    #[test]
    fn an_uploads_id_is_letters_and_digits() {
        assert!(valid_id(&format!("up_{}", uuid::Uuid::new_v4().simple())));
        assert!(valid_id("9f8c1a2b3c4d4e5f"));
        assert!(!valid_id("short"));
        assert!(!valid_id("with a space"));
        assert!(!valid_id("with/slash"));
        assert!(!valid_id(&"a".repeat(129)));
    }

    /// Only an upload's own pieces stream: a whole file, or anything else,
    /// is read as every other request is.
    #[test]
    fn only_an_uploads_pieces_are_streamed() {
        let put = hyper::Method::PUT;
        assert!(streams(&put, "/v1/workspaces/w/uploads/up_1"));
        assert!(streams(&put, "/v1/workspaces/w/uploads/up_1/"));
        assert!(!streams(&hyper::Method::POST, "/v1/workspaces/w/uploads"));
        assert!(!streams(&put, "/v1/workspaces/w/uploads"));
        assert!(!streams(&put, "/v1/workspaces/w/resources/x"));
        assert!(!streams(
            &hyper::Method::GET,
            "/v1/workspaces/w/uploads/up_1"
        ));
    }
}
