//! Content gets on an MCP session: `tilo_get` downloads a workspace file by
//! its `tilo.content:<hash>` reference, a piece at a time. Actions carry
//! intent and references; bytes come back the way they went in, in bounded
//! pieces the agent asks for again at the offset the last piece ended at.
use base64::Engine as _;
use tilo_actions::remote::{ActionHostError, CONTENT_GETS_UNAVAILABLE, ContentGets};

use super::{ActionHost, Deserialize, McpServer, Surface, Value, json, parse};

/// The get tool's name.
const GET: &str = "tilo_get";

/// The most raw bytes one call answers with.
const MAX_GET_CALL_BYTES: u64 = 512 * 1024;

pub(super) fn is_get_tool(name: &str) -> bool {
    name == GET
}

/// The get tool, offered where the host reads content.
pub(super) fn tool_definitions() -> Vec<Value> {
    vec![json!({
        "name": GET,
        "description": "Download a workspace file by its tilo.content:<hash> reference (one an upload or an action answered with): answers the bytes, base64, at offset, up to 524288 a call; call again at next_offset for the rest.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "content": {"type":"string"},
                "offset": {"type":"integer"},
                "length": {"type":"integer"},
            },
            "required": ["content"],
            "additionalProperties": false,
        },
        "annotations": {"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},
    })]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    content: String,
    #[serde(default)]
    offset: u64,
    /// Zero (or absent) asks for as many bytes as one call carries.
    #[serde(default)]
    length: u64,
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
    /// Answer the get tool, as this session's surface answers: the object as
    /// its text and, once the caller asked for typed answers (or reads the
    /// classic surface, which always carries them), as its value.
    pub(super) async fn get_call(&self, name: &str, args: Value, structured: bool) -> Value {
        let result = match name {
            GET => match parse(args) {
                Ok(args) => self.download(args).await,
                Err(error) => Err(error),
            },
            _ => Err("Unknown tool".to_owned()),
        };
        match result {
            Ok((answer, text)) => {
                if structured || self.surface == Surface::Classic {
                    json!({
                        "content":[{"type":"text","text":text}],
                        "structuredContent":answer,
                        "isError":false
                    })
                } else {
                    json!({"content":[{"type":"text","text":text}],"isError":false})
                }
            }
            Err(message) => json!({"content":[{"type":"text","text":message}],"isError":true}),
        }
    }

    /// One piece of `args.content`, and the text saying how it reads on.
    async fn download(&self, args: GetArgs) -> Result<(Value, String), String> {
        let gets: std::sync::Arc<dyn ContentGets> = self
            .host()
            .content_gets()
            .ok_or_else(|| CONTENT_GETS_UNAVAILABLE.to_owned())?;
        let length = if args.length == 0 {
            MAX_GET_CALL_BYTES
        } else {
            args.length.min(MAX_GET_CALL_BYTES)
        };
        let GetArgs {
            content, offset, ..
        } = args;
        let caller = self.caller();
        let asked = content.clone();
        let piece = tokio::task::spawn_blocking(move || gets.get(&caller, &asked, offset, length))
            .await
            .map_err(|_| "the content could not be read".to_owned())?
            .map_err(refusal)?;
        let bytes = base64::engine::general_purpose::STANDARD.encode(&piece.bytes);
        let given = piece.bytes.len();
        let answer = json!({
            "content": content,
            "offset": piece.offset,
            "length": given,
            "size": piece.size,
            "bytes": bytes,
        });
        let end = piece
            .offset
            .saturating_add(u64::try_from(given).unwrap_or(u64::MAX));
        let hint = if end < piece.size {
            format!(
                "the next bytes are at offset {end}, {} of {}: call {GET} again with offset {end}",
                piece.size.saturating_sub(end),
                piece.size
            )
        } else {
            format!("that is all of it: {} bytes", piece.size)
        };
        Ok((answer.clone(), format!("{answer} {hint}")))
    }
}

#[cfg(test)]
#[path = "mcp_get_tests.rs"]
mod tests;
