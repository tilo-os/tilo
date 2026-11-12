//! File bytes into a workspace, as one request (`POST
//! /v1/workspaces/{workspace}/uploads`), answering the `tilo.content:<hash>`
//! reference that the workspace's actions take a file by.
//!
//! It is the runtime's own upload (`tilo_put_begin`, `tilo_put_append`,
//! `tilo_put_finish`), carried in chunks by the server so a program need not
//! base64 a passport scan into MCP messages, in the key's API run.

use crate::session::Session;
use crate::{Reply, json_reply};
use base64::Engine as _;
use hyper::StatusCode;
use serde_json::{Value, json};

/// The most a file uploaded at once may be.
pub const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
/// The most raw bytes one `tilo_put_append` carries.
const CHUNK: usize = 512 * 1024;

/// The refusal of a body past [`MAX_CONTENT_BYTES`].
#[must_use]
pub fn too_large() -> Reply {
    json_reply(
        StatusCode::PAYLOAD_TOO_LARGE,
        &json!({"error": {"code": "too_large", "message": "a file uploaded at once is at most 64 MiB",
                          "next_steps": ["upload a smaller file"]}}),
    )
}

/// Upload `body` through `session` into `workspace`: its content reference.
pub(crate) async fn put(session: &Session, workspace: &str, body: &[u8]) -> Result<Value, String> {
    let answer = |result: Value, field: &str| -> Result<Value, String> {
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
    };
    let begun = session
        .call_in(workspace, "tilo_put_begin", json!({"size": body.len()}))
        .await?;
    let upload = answer(begun, "upload")?;
    for (index, chunk) in body.chunks(CHUNK).enumerate() {
        let appended = session
            .call_in(
                workspace,
                "tilo_put_append",
                json!({
                    "upload": upload,
                    "offset": index.saturating_mul(CHUNK),
                    "chunk": base64::engine::general_purpose::STANDARD.encode(chunk),
                }),
            )
            .await?;
        answer(appended, "written")?;
    }
    let finished = session
        .call_in(workspace, "tilo_put_finish", json!({"upload": upload}))
        .await?;
    answer(finished, "content")
}
