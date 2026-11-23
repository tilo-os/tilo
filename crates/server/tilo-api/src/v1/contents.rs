//! A workspace's content, addressed by the hash of its bytes: the one pair of
//! byte routes. `PUT …/contents/{hash}` carries the bytes, resumable from the
//! offset they reached, and `GET …/contents/{hash}` answers them, `Range` and
//! all.
//!
//! A put names the whole file by its BLAKE3 hash, so the bytes decide where
//! they go and a file goes up once whatever a caller would have called it. The
//! first request opens the put with the file's size (`Content-Range:
//! bytes 0-1023/4096`), each later one continues it from the offset it stands
//! at, and the request that completes it answers the `tilo.content:`
//! reference the workspace keeps it by — the address, and a sealing store's
//! key after a `.` — which an action takes a file by.
//!
//! The bytes travel to the workspace in the pieces a frame carries
//! ([`MAX_CONTENT_FRAME_BYTES`]), a group at a time so the link between them
//! costs one round trip per group rather than per piece. A group is awaited
//! whole before the next is sent, so the pieces on the wire never land past
//! the window the workspace accepts ahead of the bytes it holds.

use crate::lock;
use crate::session::Session;
use crate::v1::errors::ApiError;
use base64::Engine as _;
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_actions::remote::MAX_CONTENT_FRAME_BYTES;

/// The most one request's body may carry: a whole small file, or one piece of
/// a larger one.
pub const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
/// The most raw bytes one `tilo_put_append` carries: one frame's worth, so a
/// piece never has to be split again on its way to the workspace.
const CHUNK: usize = MAX_CONTENT_FRAME_BYTES;
/// How many of a put's pieces are on the wire at once: a slow link costs one
/// round trip per group of these instead of per piece.
const IN_FLIGHT: usize = 8;
/// How long a put nothing is sent to stays open.
const OPEN_FOR: Duration = Duration::from_secs(6 * 60 * 60);
/// How many puts one server keeps open at once; past it, the one left
/// untouched longest is dropped, storing nothing.
const MOST_OPEN: usize = 64;

/// The refusal of a body past [`MAX_CONTENT_BYTES`].
#[must_use]
pub fn too_large() -> ApiError {
    ApiError::new("too_large", "one request carries at most 64 MiB")
        .next("send the file in pieces with `Content-Range`")
}

/// Whether `hash` is the address a put names its bytes by: the BLAKE3 hash of
/// the file, lowercase hex.
#[must_use]
pub fn address(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `reference` as the content reference the workspace names its bytes by, or
/// `None` when it is not one: an address, with the key a sealed item opens by
/// after a `.`.
#[must_use]
pub fn reference(reference: &str) -> Option<String> {
    let (hash, key) = match reference.split_once('.') {
        Some((hash, key)) => (hash, Some(key)),
        None => (reference, None),
    };
    if !address(hash)
        || key.is_some_and(|key| {
            key.len() != 43
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return None;
    }
    Some(format!("tilo.content:{reference}"))
}

/// `Content-Range: bytes <from>-<to>/<size>` as the piece's first and last
/// byte and the file's size; `None` for a range this server does not take.
#[must_use]
pub fn range(value: &str) -> Option<(u64, u64, u64)> {
    let range = value.trim().strip_prefix("bytes ")?;
    let (span, size) = range.split_once('/')?;
    let (from, to) = span.split_once('-')?;
    let from: u64 = from.trim().parse().ok()?;
    let to: u64 = to.trim().parse().ok()?;
    let size: u64 = size.trim().parse().ok()?;
    (to >= from && to < size).then_some((from, to, size))
}

/// The refusal of a request that names no content address.
#[must_use]
pub fn bad_address(hash: &str) -> ApiError {
    ApiError::new(
        "invalid_request",
        format!(
            "`{hash}` is not a content address: lowercase BLAKE3 hex, 64 characters, and a sealed item's key after a `.`"
        ),
    )
    .next("a put names the hash of the file's bytes")
}

/// The refusal of a put whose bytes are not the ones its address names.
fn wrong_bytes(content: &str, hash: &str) -> ApiError {
    ApiError::new(
        "upload_failed",
        format!("the bytes are {content}, not the content address {hash}"),
    )
    .next("address the put by the hash of the whole file's bytes")
}

/// The refusal of a piece the workspace would not take.
fn put_refused(why: &str) -> ApiError {
    ApiError::new("upload_failed", why)
        .next("send the rest from where the put stands, or start again at 0")
}

/// The refusal of a piece sent from an offset the put is not at.
fn offset_mismatch(from: u64, stands: u64, size: u64) -> ApiError {
    ApiError::new(
        "offset_mismatch",
        format!("the put stands at {stands} of its {size} bytes, not {from}"),
    )
    .next(format!(
        "send this piece from byte {stands}, or start the put again at 0"
    ))
}

/// The refusal of a piece of content whose every byte is already there.
fn upload_complete(workspace: &str, content: &str) -> ApiError {
    let reference = content.strip_prefix("tilo.content:").unwrap_or(content);
    ApiError::new(
        "upload_complete",
        format!("every byte of that content is there: {content}"),
    )
    .next(format!(
        "GET /v1/workspaces/{workspace}/contents/{reference} reads it"
    ))
}

/// `field` of one of the content tools' results.
fn answer(result: &Value, field: &str) -> Result<Value, String> {
    result
        .get(field)
        .cloned()
        .ok_or_else(|| format!("the workspace answered no {field}: {result}"))
}

/// `tool` with `arguments` in `workspace`, through `session`.
async fn call_tool(
    session: &Session,
    workspace: &str,
    tool: &str,
    arguments: Value,
) -> Result<Value, ApiError> {
    session.typed(Some(workspace), tool, arguments, None).await
}

/// Open a put of `size` bytes in `workspace`: the upload the workspace
/// answers its pieces by.
async fn begin(session: &Session, workspace: &str, size: u64) -> Result<Value, ApiError> {
    let begun = call_tool(session, workspace, "tilo_put_begin", json!({"size": size}))
        .await
        .map_err(|refused| {
            if refused.body.code == "key_revoked" {
                refused
            } else {
                ApiError::new("unavailable", refused.body.message).next("open the put again")
            }
        })?;
    answer(&begun, "upload").map_err(|why| ApiError::new("unavailable", why))
}

/// Store what `upload` holds, answering its `tilo.content:<hash>` reference.
async fn finish(session: &Session, workspace: &str, upload: &Value) -> Result<String, ApiError> {
    let finished = call_tool(
        session,
        workspace,
        "tilo_put_finish",
        json!({"upload": upload}),
    )
    .await
    .map_err(|refused| {
        if refused.body.code == "key_revoked" {
            refused
        } else {
            put_refused(&refused.body.message)
        }
    })?;
    answer(&finished, "content")
        .map_err(|why| put_refused(&why))?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| put_refused("the workspace named no content"))
}

/// Send `chunk` from `offset` to `upload` in `workspace`, in the pieces one
/// `tilo_put_append` carries, a group at a time: the link is kept busy rather
/// than waiting an answer for each piece. The workspace accepts pieces ahead
/// of the bytes it holds within a bounded window and appends them as the gap
/// fills, so pieces may arrive in any order; a group is awaited whole before
/// the next is sent, so the pieces on the wire never land past that window.
/// Answers the highest contiguous count of bytes the workspace holds.
async fn send(
    session: &Session,
    workspace: &str,
    upload: &Value,
    offset: u64,
    chunk: &[u8],
) -> Result<u64, String> {
    let mut written = offset;
    let mut at = offset;
    let mut group = Vec::with_capacity(IN_FLIGHT);
    for (index, piece) in chunk.chunks(CHUNK).enumerate() {
        group.push((at, index.saturating_mul(CHUNK), piece.len()));
        at = at.saturating_add(piece.len() as u64);
        if group.len() == IN_FLIGHT {
            written = written.max(
                send_group(
                    session,
                    workspace,
                    upload,
                    chunk,
                    std::mem::take(&mut group),
                )
                .await?,
            );
        }
    }
    if !group.is_empty() {
        written = written.max(send_group(session, workspace, upload, chunk, group).await?);
    }
    Ok(written)
}

/// [`send`]'s group of pieces, sent at once: the highest contiguous count of
/// bytes the workspace holds after them.
async fn send_group(
    session: &Session,
    workspace: &str,
    upload: &Value,
    chunk: &[u8],
    group: Vec<(u64, usize, usize)>,
) -> Result<u64, String> {
    let answers: Vec<Result<u64, String>> = futures_util::stream::iter(group)
        .map(|(at, from, len)| async move {
            let piece = chunk
                .get(from..from.saturating_add(len))
                .unwrap_or_default();
            let appended = call_tool(
                session,
                workspace,
                "tilo_put_append",
                json!({
                    "upload": upload,
                    "offset": at,
                    "chunk": base64::engine::general_purpose::STANDARD.encode(piece),
                }),
            )
            .await
            .map_err(|refused| refused.body.message)?;
            let written = answer(&appended, "written")?;
            Ok::<_, String>(
                written
                    .as_u64()
                    .unwrap_or_else(|| at.saturating_add(len as u64)),
            )
        })
        .buffer_unordered(IN_FLIGHT)
        .collect()
        .await;
    let mut written = 0;
    for answer in answers {
        written = written.max(answer?);
    }
    Ok(written)
}

/// What a put answered: where an unfinished one stands, or the content its
/// last piece completed.
pub(crate) enum Put {
    More { offset: u64, size: u64 },
    Done { content: String, size: u64 },
}

/// Which put an open one is: the key's, in its workspace, by content address.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    grant: String,
    workspace: String,
    hash: String,
}

/// One put the server keeps open on its caller's behalf: the workspace's own
/// upload, where it stands, and what every byte of it is.
struct Open {
    /// The workspace's id for the upload, as `tilo_put_begin` answered it.
    upload: Value,
    /// What the put began with.
    size: u64,
    /// Bytes the workspace accepted so far.
    written: u64,
    /// The digest of the bytes accepted so far, in the order they arrived:
    /// the address the put names is the hash of the whole file, which the
    /// workspace's own reference seals and so cannot answer.
    hasher: blake3::Hasher,
    /// The reference the workspace keeps the bytes by, once every one is
    /// there: a store that seals names them by a hash of their ciphertext.
    content: Option<String>,
    touched: Instant,
}

/// The puts the server keeps open, by the content address each names.
#[derive(Default)]
pub struct Uploads {
    open: Mutex<HashMap<Key, Arc<Mutex<Open>>>>,
}

/// `put`'s bytes: `bytes` from `first`, of a file that is `total` long.
///
/// # Errors
/// The workspace refused the bytes (a key it keeps as revoked is told so,
/// typed), the piece is not the one the put stands at, or the bytes are not
/// the ones the address names.
#[expect(
    clippy::too_many_lines,
    reason = "one put, from its first piece to the content every byte of it is"
)]
pub(crate) async fn put(
    uploads: &Uploads,
    session: &Session,
    grant: &str,
    workspace: &str,
    hash: &str,
    first: u64,
    total: u64,
    body: &[u8],
) -> Result<Put, ApiError> {
    if body.is_empty() {
        return Err(ApiError::invalid("a put carries at least one byte"));
    }
    let end = first.saturating_add(body.len() as u64);
    if end > total {
        return Err(ApiError::invalid(format!(
            "this piece runs to {end} of the {total} bytes the file is"
        )));
    }
    let key = Key {
        grant: grant.to_owned(),
        workspace: workspace.to_owned(),
        hash: hash.to_owned(),
    };
    let named = lock(&uploads.open).get(&key).cloned();
    let open = if let Some(open) = named {
        open
    } else {
        if first != 0 {
            return Err(offset_mismatch(first, 0, total));
        }
        if total == 0 {
            return Err(ApiError::invalid("a put carries at least one byte"));
        }
        let upload = begin(session, workspace, total).await?;
        let open = Arc::new(Mutex::new(Open {
            upload,
            size: total,
            written: 0,
            hasher: blake3::Hasher::new(),
            content: None,
            touched: Instant::now(),
        }));
        {
            let mut all = lock(&uploads.open);
            all.retain(|_, open| lock(open).touched.elapsed() < OPEN_FOR);
            while all.len() >= MOST_OPEN {
                let Some(stalest) = all
                    .iter()
                    .min_by_key(|(_, open)| lock(open).touched)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                all.remove(&stalest);
            }
            all.insert(key.clone(), Arc::clone(&open));
        }
        open
    };
    let (size, written, upload, complete) = {
        let held = lock(&open);
        (
            held.size,
            held.written,
            held.upload.clone(),
            held.content.clone(),
        )
    };
    if let Some(content) = complete {
        // Every byte is there. A put given the whole file again is answered
        // as it was; more of it is refused with where the content is.
        if first == 0 {
            return Ok(Put::Done { content, size });
        }
        return Err(upload_complete(workspace, &content));
    }
    if first > written {
        return Err(offset_mismatch(first, written, size));
    }
    if total != size {
        return Err(ApiError::invalid(format!(
            "the put is {size} bytes, not {total}"
        )));
    }
    if first < written {
        // The bytes are there already: a piece sent again after an answer
        // that did not arrive costs nothing, and the answer says where the
        // put stands now.
        return Ok(Put::More {
            offset: written,
            size,
        });
    }
    let written = match send(session, workspace, &upload, first, body).await {
        Ok(written) => written,
        Err(why) => {
            lock(&uploads.open).remove(&key);
            return Err(put_refused(&why));
        }
    };
    let computed = {
        let mut held = lock(&open);
        // Only the bytes the workspace holds contiguously are the file's
        // prefix; the ones it accepted ahead are sent again from `written`.
        let accepted = usize::try_from(written.saturating_sub(first)).unwrap_or(usize::MAX);
        held.hasher.update(body.get(..accepted).unwrap_or(body));
        held.written = written;
        held.touched = Instant::now();
        let computed =
            (written == size).then(|| held.hasher.clone().finalize().to_hex().to_string());
        drop(held);
        computed
    };
    if let Some(computed) = computed
        && computed != hash
    {
        lock(&uploads.open).remove(&key);
        return Err(wrong_bytes(&format!("tilo.content:{computed}"), hash));
    }
    if written < size {
        return Ok(Put::More {
            offset: written,
            size,
        });
    }
    let content = match finish(session, workspace, &upload).await {
        Ok(content) => content,
        Err(refused) => {
            lock(&uploads.open).remove(&key);
            return Err(refused);
        }
    };
    {
        let mut held = lock(&open);
        held.content = Some(content.clone());
        held.touched = Instant::now();
    }
    Ok(Put::Done { content, size })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An address is 64 lowercase hex, and the reference is built from it.
    #[test]
    fn an_address_is_lowercase_hex_and_the_reference_names_it() {
        let hash = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
        let key = "A".repeat(43);
        assert!(address(hash));
        assert_eq!(
            reference(hash).as_deref(),
            Some(format!("tilo.content:{hash}").as_str())
        );
        assert_eq!(
            reference(&format!("{hash}.{key}")).as_deref(),
            Some(format!("tilo.content:{hash}.{key}").as_str())
        );
        assert!(reference(&hash.to_uppercase()).is_none());
        assert!(reference("af13").is_none());
        assert!(reference(&"g".repeat(64)).is_none());
        assert!(reference(&"a".repeat(65)).is_none());
        assert!(reference(&format!("{hash}.{}", "A".repeat(42))).is_none());
    }

    /// A put's range says where its piece starts and ends, and how big the
    /// file is; one past the file, or backwards, is not one.
    #[test]
    fn a_range_names_a_piece_of_the_file() {
        assert_eq!(range("bytes 0-1023/4096"), Some((0, 1023, 4096)));
        assert_eq!(range("bytes 1024-4095/4096"), Some((1024, 4095, 4096)));
        assert_eq!(range("bytes 0-0/1"), Some((0, 0, 1)));
        assert_eq!(range("bytes 1023-0/4096"), None);
        assert_eq!(range("bytes 0-4096/4096"), None);
        assert_eq!(range("bytes 0-1023"), None);
        assert_eq!(range("0-1023/4096"), None);
    }
}
