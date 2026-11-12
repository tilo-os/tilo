//! File bytes into a workspace, as one request: `PUT /w/<workspace>/content`
//! with the file as its body, answering the `tilo.content:<hash>` reference
//! that the workspace's actions take a file by.
//!
//! It is the runtime's own upload (`tilo_put_begin`, `tilo_put_append`,
//! `tilo_put_finish`), carried in chunks by the server so a program need not
//! base64 a passport scan into MCP messages. It goes as the grant, in the
//! caller's run when the request names its MCP session (`Mcp-Session-Id`), and
//! in a run of the grant's own otherwise.

use crate::session::{self, SESSION_HEADER, Session};
use crate::store::{Grant, Scope};
use crate::{Reply, Server, json_reply, lock, now_ms};
use base64::Engine as _;
use hyper::{Method, StatusCode};
use serde_json::{Value, json};
use std::sync::Arc;

/// The most a file uploaded at once may be.
pub const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
/// The most raw bytes one `tilo_put_append` carries.
const CHUNK: usize = 512 * 1024;
/// The server's own run for a grant's uploads in a workspace, in its session
/// table, so revoking the grant ends it like its other sessions.
const UPLOAD_SESSION: &str = "content ";

/// The workspace whose content `method` and `path` upload to.
#[must_use]
pub fn upload_target(method: &Method, path: &str) -> Option<String> {
    if *method != Method::PUT {
        return None;
    }
    let workspace = path.strip_prefix("/w/")?.strip_suffix("/content")?;
    crate::pinned_workspace(&format!("/w/{workspace}"))
}

fn refuse(status: StatusCode, code: &str, message: &str, next: &str) -> Reply {
    json_reply(
        status,
        &json!({"error": {"code": code, "message": message, "next_steps": [next]}}),
    )
}

/// The refusal of a body past [`MAX_CONTENT_BYTES`].
#[must_use]
pub fn too_large() -> Reply {
    refuse(
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_large",
        "a file uploaded at once is at most 64 MiB",
        "upload a smaller file",
    )
}

impl Server {
    /// `PUT /w/<workspace>/content`.
    pub(crate) async fn upload(
        self: &Arc<Self>,
        workspace: &str,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Reply {
        let grant = match crate::bearer(headers).map(|token| self.store.access(token, now_ms())) {
            Some(Ok(grant)) => grant,
            Some(Err(denied)) => {
                return json_reply(StatusCode::UNAUTHORIZED, &crate::denial(denied));
            }
            None => {
                return refuse(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "send a key: Authorization: Bearer <key>",
                    "create one with `tilo keys create`",
                );
            }
        };
        if !crate::oauth::scope_reaches(&grant.scope, Some(workspace)) {
            return refuse(
                StatusCode::FORBIDDEN,
                "forbidden",
                "this key does not reach that workspace",
                "upload to the workspace this key names",
            );
        }
        let session = match self.upload_session(&grant, workspace, headers).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        match put(&session, workspace, body).await {
            Ok(content) => json_reply(
                StatusCode::CREATED,
                &json!({"content": content, "size": body.len()}),
            ),
            Err(why) => refuse(
                StatusCode::BAD_GATEWAY,
                "upload_failed",
                &why,
                "upload the file again",
            ),
        }
    }

    /// The caller's session when it names one of its grant's, else the
    /// grant's upload run in `workspace`, opened the first time.
    async fn upload_session(
        self: &Arc<Self>,
        grant: &Grant,
        workspace: &str,
        headers: &hyper::HeaderMap,
    ) -> Result<Arc<Session>, Reply> {
        if let Some(id) = headers
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
        {
            return lock(&self.sessions)
                .get(id)
                .filter(|session| session.grant == grant.id)
                .cloned()
                .ok_or_else(|| {
                    refuse(
                        StatusCode::NOT_FOUND,
                        "unknown_session",
                        "no such session of this key",
                        "initialize again, or leave Mcp-Session-Id out",
                    )
                });
        }
        let id = format!("{UPLOAD_SESSION}{}/{workspace}", grant.id);
        let open = lock(&self.sessions).get(&id).cloned();
        if let Some(open) = open {
            return Ok(open);
        }
        let unavailable = |why: &str| {
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                why,
                "try again shortly",
            )
        };
        let Ok(number) = self.store.next_run(&grant.person) else {
            return Err(unavailable("this key cannot act now"));
        };
        let params = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "tilo-content", "version": env!("CARGO_PKG_VERSION")},
        });
        let opened = Arc::new(
            self.new_session(
                grant,
                Scope::Workspace(workspace.to_owned()),
                session::run_label(&grant.client_name, &params, number),
            )
            .await
            .map_err(|why| unavailable(&format!("the workspace could not be opened: {why}")))?,
        );
        let initialize = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":params});
        opened.initialize(&initialize).await.map_err(|refused| {
            let why = refused
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the workspace refused");
            unavailable(&format!("the workspace could not be opened: {why}"))
        })?;
        lock(&self.sessions).insert(id, Arc::clone(&opened));
        Ok(opened)
    }
}

/// Upload `body` through `session` into `workspace`: its content reference.
async fn put(session: &Session, workspace: &str, body: &[u8]) -> Result<Value, String> {
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
