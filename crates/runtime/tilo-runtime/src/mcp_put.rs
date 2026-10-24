//! Content puts on an MCP session: `tilo_put_begin`, `tilo_put_append`,
//! `tilo_put_finish` upload bytes into the session's room's content store
//! and answer `tilo.content:<hash>`, which the agent then passes to an
//! action. Actions carry intent and references; bytes travel by session
//! transport into the content store.
//!
//! The session owns its open uploads: when it ends they are dropped, and a
//! dropped upload stores nothing.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use tilo_actions::remote::{
    ActionHostError, CONTENT_PUTS_UNAVAILABLE, ContentPut, MAX_CONTENT_PUT_CHUNK,
    MAX_OPEN_CONTENT_PUTS,
};
use tilo_lock::MutexRecover as _;

use super::{ActionHost, Deserialize, McpServer, Surface, Value, json, parse, tool_result};

/// The longest base64 text of a chunk within [`MAX_CONTENT_PUT_CHUNK`].
const MAX_CHUNK_TEXT: usize = MAX_CONTENT_PUT_CHUNK.div_ceil(3) * 4;

/// The put tools' names.
const BEGIN: &str = "tilo_put_begin";
const APPEND: &str = "tilo_put_append";
const FINISH: &str = "tilo_put_finish";

pub(super) fn is_put_tool(name: &str) -> bool {
    matches!(name, BEGIN | APPEND | FINISH)
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
            "Append the next chunk of an upload: offset is the bytes sent so far; chunk is base64 of at most 524288 bytes.",
            json!({"upload":integer,"offset":integer,"chunk":{"type":"string"}}),
            json!(["upload", "offset", "chunk"]),
        ),
        (
            FINISH,
            "Finish an upload; answers its tilo.content:<hash> reference.",
            json!({"upload":integer}),
            json!(["upload"]),
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
type Open = Arc<Mutex<Option<Box<dyn ContentPut>>>>;

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

/// A host's refusal in its own words.
fn refusal(error: ActionHostError) -> String {
    match error {
        ActionHostError::Refused { reason }
        | ActionHostError::InvalidRequest { reason }
        | ActionHostError::Unavailable { reason } => reason,
        other => other.to_string(),
    }
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// Answer one put tool, as this session's surface answers.
    pub(super) async fn put_call(&self, name: &str, args: Value) -> Value {
        let result = match name {
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
            _ => Err("Unknown tool".to_owned()),
        };
        match self.surface {
            Surface::Classic => tool_result(result),
            Surface::Agent => {
                let (text, failed) = match result {
                    Ok(value) => (value.to_string(), false),
                    Err(message) => (message, true),
                };
                json!({"content":[{"type":"text","text":text}],"isError":failed})
            }
        }
    }

    async fn put_begin(&self, args: BeginArgs) -> Result<Value, String> {
        let puts = self
            .host()
            .content_puts()
            .ok_or_else(|| CONTENT_PUTS_UNAVAILABLE.to_owned())?;
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
        uploads.open.insert(id, Arc::new(Mutex::new(Some(begun))));
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
                    .append(offset, &chunk)
                    .map(|written| (written, open.size())),
                None => Err(ActionHostError::InvalidRequest {
                    reason: format!("no upload {id} is open in this session"),
                }),
            };
            drop(slot);
            appended
        })
        .await
        .map_err(|_| "the chunk could not be appended".to_owned())?
        .map_err(|error| {
            if matches!(error, ActionHostError::Refused { .. }) {
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
            let gone = || ActionHostError::InvalidRequest {
                reason: format!("no upload {id} is open in this session"),
            };
            let open = upload.as_ref().ok_or_else(gone)?;
            // Short of its size, the upload stays open to be completed.
            if open.written() != open.size() {
                return Err(ActionHostError::InvalidRequest {
                    reason: format!(
                        "the upload has {} of {} bytes: append the rest at offset {}",
                        open.written(),
                        open.size(),
                        open.written()
                    ),
                });
            }
            let taken = upload.take().ok_or_else(gone)?;
            drop(upload);
            taken.finish()
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
#[path = "mcp_put_tests.rs"]
mod tests;
