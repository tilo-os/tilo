//! Which servers keep a workspace, as the path `workspace/servers` of every
//! workspace a key acts in.
//!
//! - `workspace/servers#list` (or `tilo_read workspace/servers`) answers the
//!   servers keeping it: the one asked first;
//! - `workspace/servers#add {origin}` has the server whose site is at `origin`
//!   (`https://tilo.sh`) keep it as well: both keep it, a change on either
//!   reaches the other, and its members reach it at either. That server takes
//!   it as its own rules say: only for an owner of it it let in, if it has capacity;
//! - `workspace/servers#remove {server}` has a server, named by its site or by
//!   its device key as the list names it, keep it no more;
//! - `workspace/servers#move {server, origin}` moves it from `server` to the
//!   server whose site is at `origin`: that one keeps it as well first, then
//!   `server` keeps it no more. If `origin` refuses, it stays where it was.
//!
//! All are the workspace's own governance actions (`tilo.workspace`'s
//! `server.list`, `server.add`, `server.remove` and `server.move`), asked of
//! the server the key's session reaches, and only its owners have them: from a
//! device of theirs, or a key they created reaching all their workspaces. Each
//! answers the servers as they are then.

use super::Session;
use crate::account::{Outcome, answer, failed};
use serde_json::{Map, Value, json};

/// The path of a workspace's servers.
pub const ROOT: &str = "workspace/servers";
/// The workspace's governance actions this path asks.
const LIST: &str = "server.list";
const ADD: &str = "server.add";
const REMOVE: &str = "server.remove";
const MOVE: &str = "server.move";
/// What a refusal names as the next step.
const OWNERS_ONLY: &str = "an owner of the workspace chooses its servers, from a device of theirs \
                           or a key they created reaching all their workspaces; the other server \
                           keeps it only for an owner it let in: sign in there first";

/// Whether `tool` with `arguments` is about the workspace's servers.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    matches!(tool, "tilo_do" | "tilo_read")
        && arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.split('#').next() == Some(ROOT))
}

impl Session {
    /// Answer `tool` with `arguments` on `workspace`'s servers, typed when
    /// `structured`.
    pub(super) async fn hosts(
        &self,
        workspace: &str,
        tool: &str,
        arguments: &Value,
        structured: bool,
    ) -> Value {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = arguments.get("args").cloned().unwrap_or_else(|| json!({}));
        let text = |name: &str| {
            args.get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        };
        let outcome = match (tool, path) {
            ("tilo_do", "workspace/servers#list") | ("tilo_read", ROOT) => {
                self.ask_hosts(workspace, LIST, Map::new()).await
            }
            ("tilo_do", "workspace/servers#add") => match text("origin") {
                Some(origin) => {
                    let mut asked = Map::new();
                    asked.insert("origin".into(), json!(origin));
                    self.ask_hosts(workspace, ADD, asked).await
                }
                None => failed(
                    "invalid_arguments",
                    "`origin` is the other server's site",
                    "pass args {origin: \"https://tilo.sh\"}",
                ),
            },
            ("tilo_do", "workspace/servers#remove") => match text("server") {
                Some(server) => {
                    let mut asked = Map::new();
                    asked.insert("server".into(), json!(server));
                    self.ask_hosts(workspace, REMOVE, asked).await
                }
                None => failed(
                    "invalid_arguments",
                    "`server` is the server's site, or its device key as workspace/servers#list names it",
                    "pass args {server: \"https://tilo.sh\"}",
                ),
            },
            ("tilo_do", "workspace/servers#move") => match (text("server"), text("origin")) {
                (Some(server), Some(origin)) => {
                    let mut asked = Map::new();
                    asked.insert("server".into(), json!(server));
                    asked.insert("origin".into(), json!(origin));
                    self.ask_hosts(workspace, MOVE, asked).await
                }
                _ => failed(
                    "invalid_arguments",
                    "`server` is the server it moves from, `origin` the site of the one it moves to",
                    "pass args {server: \"<device key or site>\", origin: \"https://tilo.sh\"}",
                ),
            },
            (_, other) => failed(
                "not_found",
                format!("the workspace has no action {other}"),
                "its actions are workspace/servers#list, workspace/servers#add with the other \
                 server's `origin`, workspace/servers#remove with the `server` to let go, and \
                 workspace/servers#move with the `server` it moves from and the `origin` it \
                 moves to",
            ),
        };
        answer(structured, path, outcome)
    }

    /// The workspace's servers after its governance action `verb`.
    async fn ask_hosts(&self, workspace: &str, verb: &str, asked: Map<String, Value>) -> Outcome {
        match self.govern_or(workspace, verb, asked, OWNERS_ONLY).await {
            Ok(hosts) => Outcome::Ok(hosts),
            Err(outcome) => outcome,
        }
    }
}
