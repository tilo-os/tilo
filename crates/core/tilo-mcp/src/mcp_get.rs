//! Content gets on an MCP session: `tilo_get` downloads a workspace file by
//! its `tilo.content:<hash>` reference, a piece at a time. Actions carry
//! intent and references; bytes come back the way they went in, in bounded
//! pieces the agent asks for again at the offset the last piece ended at.
use base64::Engine as _;
use tilo_actions::remote::{ActionCallError, CONTENT_GETS_UNAVAILABLE, ContentGets};

use super::{ActionProvider, Deserialize, McpServer, Value, json, parse};

/// The get tool's name.
pub(super) const GET: &str = "tilo_get";

/// The most raw bytes one call answers with: one piece of a content frame, which no call may
/// ask past ([`tilo_actions::remote::MAX_CONTENT_FRAME_BYTES`]).
pub(super) const MAX_GET_CALL_BYTES: u64 = tilo_actions::remote::MAX_CONTENT_FRAME_BYTES as u64;

pub(super) fn is_get_tool(name: &str) -> bool {
    name == GET
}

/// The get tool, offered where the host reads content.
pub(super) fn tool_definitions() -> Vec<Value> {
    vec![json!({
        "name": GET,
        "description": "Download a workspace file by its tilo.content:<hash> reference (one an upload or an action answered with): answers the bytes, base64, at offset, up to 393216 a call; call again at next_offset for the rest.",
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
fn refusal(error: ActionCallError) -> String {
    match error {
        ActionCallError::Refused { reason }
        | ActionCallError::InvalidRequest { reason }
        | ActionCallError::Unavailable { reason } => reason,
        other => other.to_string(),
    }
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Answer the get tool, as this session's surface answers: the object as
    /// its text and, once the caller asked for typed answers (or reads the
    /// classic surface, which always carries them), as its value.
    pub(super) async fn get_call(&self, name: &str, args: Value, structured: bool) -> Value {
        match self.get_value(name, args).await {
            Ok((answer, text)) => {
                if structured {
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

    /// The get tool's typed piece and the words saying how it reads on, or why there is none.
    pub(super) async fn get_value(
        &self,
        name: &str,
        args: Value,
    ) -> Result<(Value, String), String> {
        match name {
            GET => self.download(parse(args)?).await,
            _ => Err("Unknown tool".to_owned()),
        }
    }

    /// What reads the workspace's contents piece by piece, where this connection has it.
    pub(crate) fn workspace_gets(&self) -> Option<std::sync::Arc<dyn ContentGets>> {
        self.host().content_gets()
    }

    /// One piece of `args.content`, and the text saying how it reads on.
    async fn download(&self, args: GetArgs) -> Result<(Value, String), String> {
        let gets: std::sync::Arc<dyn ContentGets> = self
            .workspace_gets()
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
