//! `tilo-sdk-native`: the TypeScript SDK's way into Tilo, the device daemon's native endpoint.
//!
//! A program connects to this computer's daemon as `tilo`'s workspace commands do
//! ([`tilo_api::calls::Calls`]): action frames ([`tilo_actions::remote::RemoteActionFrame`]),
//! one per line, carried by the line wire's one client, walked as the agent surface's tools in
//! this process. The daemon admits the connection as whoever makes it: the person's own
//! account, or an app process the daemon started, in the one workspace it was started in.
//!
//! Every call settles a promise on napi's tokio runtime with `{answer}` or `{refused}`, the
//! refusal's body as the SDK raises its typed errors from. The Automerge bytes a call reads are
//! decoded in sandboxed workers, started as the `tilo-automerge-worker` program beside the addon.

// The calls' futures nest deeply enough (the MCP walk among them) that proving them `Send`
// passes the default limit of 128.
#![recursion_limit = "256"]

use napi_derive::napi;
use std::sync::Arc;
use tilo_api::calls::Calls;

/// Start the client's Automerge workers as `program`, the `tilo-automerge-worker` program the
/// platform package carries beside the addon: Node cannot be started again as one. `index.js`
/// names it before the first connection.
#[napi]
pub fn serve_workers_from(program: String) {
    tilo_automerge_isolate::serve_workers_from(program.into());
}

/// A connection to this computer's daemon.
#[napi]
pub struct Daemon {
    calls: Arc<Calls>,
}

/// Connect to the daemon's native endpoint ([`Calls::connect`]): the connection the launcher
/// handed an app it started, else `socket`, else this computer's daemon. `workspace` names the one
/// workspace the connection acts in when it knows it.
#[napi]
pub async fn connect(socket: Option<String>, workspace: Option<String>) -> napi::Result<Daemon> {
    let socket = socket.map(std::path::PathBuf::from);
    let calls = Calls::connect(socket.as_deref(), workspace.as_deref())
        .await
        .map_err(|error| napi::Error::from_reason(format!("{error:#}")))?;
    Ok(Daemon {
        calls: Arc::new(calls),
    })
}

#[napi]
impl Daemon {
    /// The workspace this connection acts in when it names none.
    #[napi(getter)]
    pub fn workspace(&self) -> Option<String> {
        self.calls.served_workspace().map(str::to_owned)
    }

    /// Call `tool` with `args` in `workspace` (`null`: the account's own tree), as a step of
    /// `group` (`{id, title}`) when one is named: `{answer}`, or `{refused: {error: {...}}}`.
    #[napi]
    pub async fn call(
        &self,
        workspace: Option<String>,
        tool: String,
        args: serde_json::Value,
        group: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let calls = Arc::clone(&self.calls);
        match calls.call(workspace.as_deref(), &tool, args, group).await {
            Ok(answer) => serde_json::json!({ "answer": answer }),
            Err(refused) => serde_json::json!({ "refused": refused }),
        }
    }

    /// The secret of the vault item `item` in `workspace` (`null`: the one this connection acts
    /// in), released to this process ([`Calls::release`]): `{answer: {secret}}`, or `{refused}`.
    /// The secret goes to the caller alone.
    #[napi]
    pub async fn release(&self, workspace: Option<String>, item: String) -> serde_json::Value {
        let calls = Arc::clone(&self.calls);
        match calls.release(workspace.as_deref(), &item).await {
            Ok(answer) => serde_json::json!({ "answer": answer }),
            Err(refused) => serde_json::json!({ "refused": refused }),
        }
    }
}
