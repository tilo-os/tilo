//! `tilo mcp --as-me`: the same tools over stdio, called as the person rather than as an agent
//! they admit. A program of theirs that is no agent speaks to their workspace through it (the
//! `tilolang repl`, run as its own program): each call goes where `tilo do` sends it
//! ([`tilo_api::calls::Calls`]: the key `--key-file` names, or this computer's daemon), and what
//! it changes is the person's in History and undo, as `tilo do`'s is.
//!
//! This is no wider than `tilo do`: any program of this login runs that as the person already.
//! One server keeps one program session: the `tilo` tool's `let`s and references carry from one
//! call to the next for as long as it runs.
//!
//! A call acts in the workspace its `workspace` argument names, else the default one: the one
//! `--workspace` names, else the one this computer's login entered. The initialize answer names
//! that workspace (`_meta["so.tilo.mcp"].workspace`, `{id, name}`); with none, a call naming no
//! workspace acts on the person's account (`account/workspaces#list` lists their workspaces).

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use serde_json::{Value, json};
use tilo_api::calls::{Calls, Reach, Refusal};

/// Why the server serves nothing, in the initialize error's `data.reason`: nothing on this
/// computer signs the person in.
pub const NOT_SIGNED_IN: &str = "not_signed_in";

/// JSON-RPC's code for a server that cannot serve: what `initialize` answers then.
const UNAVAILABLE: i64 = -32002;

/// What the server serves: the person's calls and the workspace they default to, or why it
/// serves none.
enum Opened {
    Served {
        calls: Calls,
        workspace: Option<(String, String)>,
    },
    Refused {
        reason: &'static str,
        message: String,
    },
}

/// `tilo mcp --as-me`, acting as `key_file`'s key when one is named, in `workspace` when one is
/// named: JSON-RPC lines on stdin answered on stdout until stdin ends.
///
/// # Errors
/// Standard input or output failed.
pub fn serve(key_file: Option<&Path>, workspace: Option<&str>) -> Result<ExitCode> {
    let runtime = crate::login::runtime()?;
    let opened = runtime.block_on(open(key_file, workspace));
    super::answering(|line| match serde_json::from_str::<Value>(line) {
        Ok(request) => runtime
            .block_on(answer(&opened, &request))
            .map(|answer| answer.to_string()),
        Err(error) => Some(failure(&Value::Null, -32700, &error.to_string(), None).to_string()),
    })
}

/// The calls `key_file` (or this computer's login) reaches, and the workspace they act in.
async fn open(key_file: Option<&Path>, named: Option<&str>) -> Opened {
    let reach = Reach {
        key_file,
        json: false,
    };
    let signed_in = key_file.is_some() || crate::login::desktop_runs() || crate::login::logged_in();
    if !signed_in {
        return Opened::Refused {
            reason: NOT_SIGNED_IN,
            message: "You're not signed in to Tilo on this computer: sign in with `tilo login`."
                .to_owned(),
        };
    }
    let refused = |error: anyhow::Error| Opened::Refused {
        reason: "unavailable",
        message: error.to_string(),
    };
    let calls = match Box::pin(Calls::open_with(reach, super::reach_daemon)).await {
        Ok(calls) => calls,
        Err(error) => return refused(error),
    };
    let workspace = if let Some(named) = named {
        match Box::pin(calls.workspace_id(named)).await {
            Ok(id) => Some((id, named.to_owned())),
            Err(error) => return refused(error),
        }
    } else {
        match tilo_api::calls::entered_workspace(reach) {
            Ok(entered) => entered,
            Err(error) => return refused(error),
        }
    };
    Opened::Served { calls, workspace }
}

/// The answer to `request`, or `None` for a notification.
async fn answer(opened: &Opened, request: &Value) -> Option<Value> {
    let id = request.get("id")?;
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or_default();
    let (calls, workspace) = match opened {
        Opened::Served { calls, workspace } => (calls, workspace),
        Opened::Refused { reason, message } => {
            return Some(match method {
                "initialize" => failure(id, UNAVAILABLE, message, Some(json!({"reason": reason}))),
                "ping" => success(id, &json!({})),
                "tools/list" => success(id, &json!({"tools": []})),
                "tools/call" => success(id, &called(Err(anyhow::anyhow!("{message}")))),
                other => failure(id, -32601, &format!("no method {other}"), None),
            });
        }
    };
    Some(match method {
        "initialize" => success(id, &initialized(&params, workspace.as_ref())),
        "ping" => success(id, &json!({})),
        "tools/list" => match Box::pin(calls.tools()).await {
            Ok(tools) => success(id, &json!({"tools": tools})),
            Err(error) => failure(id, UNAVAILABLE, &error.to_string(), None),
        },
        "tools/call" => {
            let Some(tool) = params.get("name").and_then(Value::as_str) else {
                return Some(failure(id, -32602, "a tools/call names its tool", None));
            };
            let mut arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let asked = arguments
                .as_object_mut()
                .and_then(|fields| fields.remove(tilo_api::session::WORKSPACE_ARGUMENT))
                .and_then(|named| named.as_str().map(str::to_owned));
            let workspace = asked.or_else(|| workspace.as_ref().map(|(id, _)| id.clone()));
            let group = params.pointer("/_meta/so.tilo.mcp/invocation").cloned();
            let answer = Box::pin(calls.typed(workspace.as_deref(), tool, arguments, group)).await;
            success(id, &called(answer))
        }
        other => failure(id, -32601, &format!("no method {other}"), None),
    })
}

/// The initialize answer: the server, and the workspace its calls act in.
fn initialized(params: &Value, workspace: Option<&(String, String)>) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2025-06-18");
    let mut answer = json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "tilo", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "These tools act in your Tilo workspace as you, as `tilo do` does: what \
                         they change is yours in History and undo.",
    });
    if let (Some(fields), Some((id, name))) = (answer.as_object_mut(), workspace) {
        fields.insert(
            "_meta".into(),
            json!({"so.tilo.mcp": {"workspace": {"id": id, "name": name}}}),
        );
    }
    answer
}

/// A `tools/call` result for `answer`: its words, and the typed answer a program reads, or the
/// refusal (`{error: {code, message}}`) when it was refused.
fn called(answer: Result<Value>) -> Value {
    match answer {
        Ok(typed) => {
            let text = typed
                .get("text")
                .and_then(Value::as_str)
                .map_or_else(|| typed.to_string(), str::to_owned);
            json!({
                "content": [{"type": "text", "text": text}],
                "structuredContent": typed,
                "isError": false,
            })
        }
        Err(error) => {
            let code = Refusal::of(&error).map_or("failed", Refusal::code);
            let message = error.to_string();
            json!({
                "content": [{"type": "text", "text": message}],
                "structuredContent": {"error": {"code": code, "message": message}},
                "isError": true,
            })
        }
    }
}

fn success(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn failure(id: &Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": message});
    if let (Some(fields), Some(data)) = (error.as_object_mut(), data) {
        fields.insert("data".into(), data);
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(opened: &Opened, request: &Value) -> Option<Value> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(answer(opened, request))
    }

    /// With nothing signing the person in, the server says so where a program first asks, and
    /// why, so the program words it for the person; a call answers the same words.
    #[test]
    fn a_server_nobody_signed_in_to_says_so_at_initialize() {
        let refused = Opened::Refused {
            reason: NOT_SIGNED_IN,
            message: "sign in with `tilo login`".into(),
        };
        let init = ask(
            &refused,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        )
        .unwrap();
        assert_eq!(init["error"]["data"]["reason"], NOT_SIGNED_IN);
        assert!(
            init["error"]["message"]
                .as_str()
                .unwrap()
                .contains("tilo login")
        );
        assert!(
            ask(
                &refused,
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
            )
            .is_none()
        );
        let call = ask(
            &refused,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "tilo", "arguments": {"program": "1"}}}),
        )
        .unwrap();
        assert_eq!(call["result"]["isError"], true);
        assert_eq!(
            call["result"]["structuredContent"]["error"]["message"],
            "sign in with `tilo login`"
        );
    }

    /// The initialize answer names the workspace calls act in, so a program needs not ask.
    #[test]
    fn the_initialize_answer_names_the_default_workspace() {
        let named = initialized(
            &json!({"protocolVersion": "2025-03-26"}),
            Some(&("3ee4a797".into(), "Home".into())),
        );
        assert_eq!(named["protocolVersion"], "2025-03-26");
        assert_eq!(
            named["_meta"]["so.tilo.mcp"]["workspace"],
            json!({"id": "3ee4a797", "name": "Home"})
        );
        assert!(initialized(&json!({}), None).get("_meta").is_none());
    }
}
