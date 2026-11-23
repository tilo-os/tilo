//! Content puts on an MCP session: `tilo_put_begin`, `tilo_put_append`,
//! `tilo_put_finish` upload bytes into the session's workspace's content store
//! and answer `tilo.content:<hash>`, which the agent then passes to an
//! action. Actions carry intent and references; bytes travel by session
//! transport into the content store.
//!
//! The session owns its open uploads: when it ends they are dropped, and a
//! dropped upload stores nothing.
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use tilo_actions::remote::{
    ActionCallError, CONTENT_PUTS_UNAVAILABLE, ContentPut, ContentPuts, MAX_CONTENT_PUT_AHEAD,
    MAX_CONTENT_PUT_CHUNK, MAX_HAVE_HASHES, MAX_OPEN_CONTENT_PUTS, MAX_PUT_MANY_BYTES,
};
use tilo_lock::MutexRecover as _;

use super::{ActionProvider, Deserialize, McpServer, Value, json, parse};

/// The longest base64 text of a chunk within [`MAX_CONTENT_PUT_CHUNK`].
const MAX_CHUNK_TEXT: usize = MAX_CONTENT_PUT_CHUNK.div_ceil(3) * 4;

/// The most raw bytes one many-files call takes: a few steps of
/// [`MAX_PUT_MANY_BYTES`], under the request body a caller can send.
const MAX_MANY_BYTES: usize = MAX_PUT_MANY_BYTES * 2;

/// The most hashes one call asks about.
const MAX_HASHES: usize = MAX_HAVE_HASHES * 8;

/// The put tools' names.
const BEGIN: &str = "tilo_put_begin";
const APPEND: &str = "tilo_put_append";
const FINISH: &str = "tilo_put_finish";
const MANY: &str = "tilo_put_many";
const HAVE: &str = "tilo_put_have";

pub(super) fn is_put_tool(name: &str) -> bool {
    matches!(name, BEGIN | APPEND | FINISH | MANY | HAVE)
}

/// The put tools, offered where the host has a content store.
pub(super) fn tool_definitions() -> Vec<Value> {
    let integer = json!({"type":"integer"});
    let annotations = json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":false,"openWorldHint":false});
    [
        (
            BEGIN,
            "Upload bytes for an action to use (a picture, a file): begin with their total size, append them in order, finish to get a tilo.content:<hash> reference to pass as an action's argument.",
            json!({"size":integer}),
            json!(["size"]),
        ),
        (
            APPEND,
            "Append a chunk of an upload: offset is where its bytes belong (chunks may be sent several ahead of the bytes the upload holds, and arrive in any order); chunk is base64 of at most 524288 bytes.",
            json!({"upload":integer,"offset":integer,"chunk":{"type":"string"}}),
            json!(["upload", "offset", "chunk"]),
        ),
        (
            FINISH,
            "Finish an upload; answers its tilo.content:<hash> reference.",
            json!({"upload":integer}),
            json!(["upload"]),
        ),
        (
            MANY,
            "Store several small files whole in one call, each base64; answers each one's tilo.content:<hash> reference in order.",
            json!({"files":{"type":"array","items":{"type":"string"}}}),
            json!(["files"]),
        ),
        (
            HAVE,
            "Which of these content hashes (lowercase BLAKE3 hex) the workspace already holds, in order.",
            json!({"hashes":{"type":"array","items":{"type":"string"}}}),
            json!(["hashes"]),
        ),
    ]
    .into_iter()
    .map(|(name, description, properties, required)| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations": annotations,
        })
    })
    .collect()
}

/// An open upload; `None` once finished. Each call holds it while it works,
/// so a finish waits for an append still running.
type Open = Arc<Mutex<Option<Upload>>>;

/// One upload in progress: its store-side put, and the chunks that arrived
/// ahead of the bytes it holds, waiting for the gap before them to fill.
struct Upload {
    put: Box<dyn ContentPut>,
    /// Chunks accepted ahead of `put.written()`, by offset, each landing
    /// within [`MAX_CONTENT_PUT_AHEAD`] of it and overlapping no other.
    ahead: BTreeMap<u64, Vec<u8>>,
}

impl Upload {
    /// Accept `chunk` at `offset`: append it now when it continues the bytes
    /// held, hold it when it lands within the window ahead of them, and
    /// refuse it when it overlaps bytes the upload holds already or lands
    /// past the window. Answers the bytes held contiguously from the start,
    /// so pieces sent several at a time over a slow link cost that link one
    /// round trip per window, not per piece.
    fn accept(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        let (written, size) = (self.put.written(), self.put.size());
        let end = offset.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if end > size {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("the upload began with {size} bytes"),
            });
        }
        if offset < written {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("the upload has {written} bytes: append at offset {written}"),
            });
        }
        if offset > written {
            let window = written.saturating_add(MAX_CONTENT_PUT_AHEAD as u64);
            if end > window {
                return Err(ActionCallError::InvalidRequest {
                    reason: format!(
                        "that chunk lands {end} bytes in: at most \
                         {MAX_CONTENT_PUT_AHEAD} bytes ahead of the {written} the upload holds"
                    ),
                });
            }
        }
        // The held chunks are disjoint: the last one starting before `end`
        // is the only one that can reach into this chunk's bytes.
        if self
            .ahead
            .range(..end)
            .next_back()
            .is_some_and(|(at, held)| at.saturating_add(held.len() as u64) > offset)
        {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("the upload already holds those bytes: append at offset {written}"),
            });
        }
        if offset > written {
            self.ahead.insert(offset, chunk.to_vec());
            return Ok(written);
        }
        let after = self.put.append(offset, chunk)?;
        self.drain(after)
    }

    /// Append the chunks held from `written` on, as far as they reach
    /// contiguously; answers the bytes held after them.
    fn drain(&mut self, written: u64) -> Result<u64, ActionCallError> {
        let mut at = written;
        while let Some(piece) = self.ahead.remove(&at) {
            at = self.put.append(at, &piece)?;
        }
        Ok(at)
    }
}

/// A session's open uploads, by the id it was given for each.
#[derive(Default)]
pub(super) struct Uploads {
    last: u64,
    open: HashMap<u64, Open>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginArgs {
    size: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppendArgs {
    upload: u64,
    offset: u64,
    chunk: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishArgs {
    upload: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManyArgs {
    files: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HaveArgs {
    hashes: Vec<String>,
}

/// A host's refusal in its own words.
fn refusal(error: ActionCallError) -> String {
    match error {
        ActionCallError::Refused { reason }
        | ActionCallError::InvalidRequest { reason }
        | ActionCallError::Unavailable { reason } => reason,
        other => other.to_string(),
    }
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Answer one put tool, as this session's surface answers.
    pub(super) async fn put_call(&self, name: &str, args: Value) -> Value {
        let (text, failed) = match self.put_value(name, args).await {
            Ok(value) => (value.to_string(), false),
            Err(message) => (message, true),
        };
        json!({"content":[{"type":"text","text":text}],"isError":failed})
    }

    /// One put tool's typed result, or why it put nothing.
    pub(super) async fn put_value(&self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            BEGIN => match parse(args) {
                Ok(args) => self.put_begin(args).await,
                Err(error) => Err(error),
            },
            APPEND => match parse(args) {
                Ok(args) => self.put_append(args).await,
                Err(error) => Err(error),
            },
            FINISH => match parse(args) {
                Ok(args) => self.put_finish(args).await,
                Err(error) => Err(error),
            },
            MANY => match parse(args) {
                Ok(args) => self.put_many(args).await,
                Err(error) => Err(error),
            },
            HAVE => match parse(args) {
                Ok(args) => self.put_have(args).await,
                Err(error) => Err(error),
            },
            _ => Err("Unknown tool".to_owned()),
        }
    }

    /// The session's content puts, when its workspace keeps any.
    fn puts(&self) -> Result<std::sync::Arc<dyn ContentPuts>, String> {
        self.host()
            .content_puts()
            .ok_or_else(|| CONTENT_PUTS_UNAVAILABLE.to_owned())
    }

    async fn put_many(&self, args: ManyArgs) -> Result<Value, String> {
        let puts = self.puts()?;
        if args.files.is_empty() {
            return Err("give at least one file".into());
        }
        let mut files = Vec::with_capacity(args.files.len());
        let mut total = 0_usize;
        for text in args.files {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text.as_bytes())
                .map_err(|_| "each file must be base64".to_owned())?;
            total = total.saturating_add(bytes.len());
            if total > MAX_MANY_BYTES {
                return Err(format!(
                    "one call stores at most {MAX_MANY_BYTES} bytes of files"
                ));
            }
            files.push(bytes);
        }
        let caller = self.caller();
        let contents = tokio::task::spawn_blocking(move || puts.put_many(&caller, &files))
            .await
            .map_err(|_| "the files could not be stored".to_owned())?
            .map_err(refusal)?;
        Ok(json!({"contents": contents}))
    }

    async fn put_have(&self, args: HaveArgs) -> Result<Value, String> {
        let puts = self.puts()?;
        if args.hashes.len() > MAX_HASHES {
            return Err(format!("one call asks about at most {MAX_HASHES} hashes"));
        }
        let caller = self.caller();
        let present = tokio::task::spawn_blocking(move || puts.have(&caller, &args.hashes))
            .await
            .map_err(|_| "the workspace could not be asked what it holds".to_owned())?
            .map_err(refusal)?;
        Ok(json!({"have": present}))
    }

    async fn put_begin(&self, args: BeginArgs) -> Result<Value, String> {
        let puts = self.puts()?;
        if self.uploads.lock_recover().open.len() >= MAX_OPEN_CONTENT_PUTS {
            return Err(format!(
                "this session already has {MAX_OPEN_CONTENT_PUTS} uploads open: finish one first"
            ));
        }
        let caller = self.caller();
        let begun = tokio::task::spawn_blocking(move || puts.begin(&caller, args.size))
            .await
            .map_err(|_| "the upload could not begin".to_owned())?
            .map_err(refusal)?;
        let id = self.keep_upload(begun)?;
        Ok(json!({"upload": id}))
    }

    /// Keep `begun` open in this session under a new id.
    fn keep_upload(&self, begun: Box<dyn ContentPut>) -> Result<u64, String> {
        let mut uploads = self.uploads.lock_recover();
        // Overlapping begins may each have passed the check before theirs.
        if uploads.open.len() >= MAX_OPEN_CONTENT_PUTS {
            return Err(format!(
                "this session already has {MAX_OPEN_CONTENT_PUTS} uploads open: finish one first"
            ));
        }
        uploads.last = uploads.last.saturating_add(1);
        let id = uploads.last;
        uploads.open.insert(
            id,
            Arc::new(Mutex::new(Some(Upload {
                put: begun,
                ahead: BTreeMap::new(),
            }))),
        );
        drop(uploads);
        Ok(id)
    }

    fn open_upload(&self, id: u64) -> Result<Open, String> {
        self.uploads
            .lock_recover()
            .open
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("no upload {id} is open in this session"))
    }

    async fn put_append(&self, args: AppendArgs) -> Result<Value, String> {
        // Base64 grows by a third: anything longer cannot decode within the bound.
        if args.chunk.len() > MAX_CHUNK_TEXT {
            return Err(too_long());
        }
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(args.chunk.as_bytes())
            .map_err(|_| "chunk must be base64".to_owned())?;
        if chunk.len() > MAX_CONTENT_PUT_CHUNK {
            return Err(too_long());
        }
        let upload = self.open_upload(args.upload)?;
        let offset = args.offset;
        let id = args.upload;
        let (written, size) = tokio::task::spawn_blocking(move || {
            let mut slot = upload.lock_recover();
            let appended = match slot.as_mut() {
                Some(open) => open
                    .accept(offset, &chunk)
                    .map(|written| (written, open.put.size())),
                None => Err(ActionCallError::InvalidRequest {
                    reason: format!("no upload {id} is open in this session"),
                }),
            };
            drop(slot);
            appended
        })
        .await
        .map_err(|_| "the chunk could not be appended".to_owned())?
        .map_err(|error| {
            if matches!(error, ActionCallError::Refused { .. }) {
                // The store refused the chunk: the upload is over.
                self.uploads.lock_recover().open.remove(&id);
            }
            refusal(error)
        })?;
        Ok(json!({"upload": args.upload, "written": written, "size": size}))
    }

    async fn put_finish(&self, args: FinishArgs) -> Result<Value, String> {
        let id = args.upload;
        let slot = self.open_upload(id)?;
        let upload = Arc::clone(&slot);
        let reference = tokio::task::spawn_blocking(move || {
            let mut upload = upload.lock_recover();
            let gone = || ActionCallError::InvalidRequest {
                reason: format!("no upload {id} is open in this session"),
            };
            let open = upload.as_ref().ok_or_else(gone)?;
            // Short of its size, the upload stays open to be completed.
            if open.put.written() != open.put.size() {
                return Err(ActionCallError::InvalidRequest {
                    reason: format!(
                        "the upload has {} of {} bytes: append the rest at offset {}",
                        open.put.written(),
                        open.put.size(),
                        open.put.written()
                    ),
                });
            }
            let taken = upload.take().ok_or_else(gone)?;
            drop(upload);
            taken.put.finish()
        })
        .await;
        let ended = slot.lock_recover().is_none();
        if ended {
            // Finished or failed, it is no longer open.
            self.uploads.lock_recover().open.remove(&id);
        }
        let reference = reference
            .map_err(|_| "the upload could not finish".to_owned())?
            .map_err(refusal)?;
        Ok(json!({"content": reference}))
    }
}

fn too_long() -> String {
    format!("a chunk carries at most {MAX_CONTENT_PUT_CHUNK} bytes: split it")
}

#[cfg(test)]
#[path = "mcp_content_tests.rs"]
mod tests;
