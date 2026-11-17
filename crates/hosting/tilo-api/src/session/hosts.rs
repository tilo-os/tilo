//! Which hosts hold a workspace, as the path `workspace/hosts` of every
//! workspace a key acts in.
//!
//! - `workspace/hosts#list` (or `tilo_read workspace/hosts`) answers the hosts
//!   holding it: the one it was made on, which governs it, first;
//! - `workspace/hosts#add {origin}` has the host whose site is at `origin`
//!   (`https://tilo.sh`) hold it as well: both keep it, a change on either
//!   reaches the other, and its members reach it at either. That host takes
//!   it as its own rules say: only for an owner of it it let in, if it has capacity;
//! - `workspace/hosts#remove {host}` has a host, named by its site or by its
//!   device key as the list names it, hold it no more.
//!
//! All are the workspace's own governance actions (`tilo.workspace`'s
//! `host.list`, `host.add` and `host.remove`), asked of the host that governs
//! it over the key's session, and only its owners have them: from a device of
//! theirs, or a key they created reaching all their workspaces. Each answers
//! the hosts as they are then.

use super::Session;
use crate::account::{Outcome, answer, failed};
use serde_json::{Map, Value, json};

/// The path of a workspace's hosts.
pub const ROOT: &str = "workspace/hosts";
/// The workspace's governance actions this path asks.
const LIST: &str = "host.list";
const ADD: &str = "host.add";
const REMOVE: &str = "host.remove";
/// What a refusal names as the next step.
const OWNERS_ONLY: &str = "an owner of the workspace chooses its hosts, from a device of theirs \
                           or a key they created reaching all their workspaces";

/// Whether `tool` with `arguments` is about the workspace's hosts.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    matches!(tool, "tilo_do" | "tilo_read")
        && arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.split('#').next() == Some(ROOT))
}

impl Session {
    /// Answer `tool` with `arguments` on `workspace`'s hosts, typed when
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
            ("tilo_do", "workspace/hosts#list") | ("tilo_read", ROOT) => {
                self.ask_hosts(workspace, LIST, Map::new()).await
            }
            ("tilo_do", "workspace/hosts#add") => match text("origin") {
                Some(origin) => {
                    let mut asked = Map::new();
                    asked.insert("origin".into(), json!(origin));
                    self.ask_hosts(workspace, ADD, asked).await
                }
                None => failed(
                    "invalid_arguments",
                    "`origin` is the other host's site",
                    "pass args {origin: \"https://tilo.sh\"}",
                ),
            },
            ("tilo_do", "workspace/hosts#remove") => match text("host") {
                Some(host) => {
                    let mut asked = Map::new();
                    asked.insert("host".into(), json!(host));
                    self.ask_hosts(workspace, REMOVE, asked).await
                }
                None => failed(
                    "invalid_arguments",
                    "`host` is the host's site, or its device key as workspace/hosts#list names it",
                    "pass args {host: \"https://tilo.sh\"}",
                ),
            },
            (_, other) => failed(
                "not_found",
                format!("the workspace has no action {other}"),
                "its actions are workspace/hosts#list, workspace/hosts#add with the other host's \
                 `origin`, and workspace/hosts#remove with the `host` to let go",
            ),
        };
        answer(structured, path, outcome)
    }

    /// The workspace's hosts after its governance action `verb`.
    async fn ask_hosts(&self, workspace: &str, verb: &str, asked: Map<String, Value>) -> Outcome {
        match self.govern_or(workspace, verb, asked, OWNERS_ONLY).await {
            Ok(hosts) => Outcome::Ok(hosts),
            Err(outcome) => outcome,
        }
    }
}
