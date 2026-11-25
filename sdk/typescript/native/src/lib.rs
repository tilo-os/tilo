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

use napi::bindgen_prelude::Buffer;
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

    /// Make the web call `args` names with the key of the vault item it names, in `workspace`
    /// (`null`: the one this connection acts in): `{answer}` or `{refused}`. An app's held
    /// connection has the vault add the key to the request; the secret never reaches the app.
    #[napi]
    pub async fn web_call(
        &self,
        workspace: Option<String>,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let serde_json::Value::Object(arguments) = args else {
            return serde_json::json!({
                "refused": {"error": {
                    "code": "invalid_request",
                    "message": "a web call names its arguments as an object",
                }},
            });
        };
        let calls = Arc::clone(&self.calls);
        match calls.web_call(workspace.as_deref(), arguments).await {
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

    /// The reference of the vault item tagged for `app` and `purpose` in `workspace` (`null`:
    /// the one this connection acts in), found on this app's own connection
    /// ([`Calls::find`]): `{answer: {reference}}` or `{answer: {}}`, or `{refused}`.
    #[napi]
    pub async fn find(
        &self,
        workspace: Option<String>,
        app: String,
        purpose: String,
    ) -> serde_json::Value {
        let calls = Arc::clone(&self.calls);
        match calls.find(workspace.as_deref(), &app, &purpose).await {
            Ok(answer) => serde_json::json!({ "answer": answer }),
            Err(refused) => serde_json::json!({ "refused": refused }),
        }
    }

    /// Carry the outside call `pair` (`{outside, tilo}`) names into the workspace call, or let go
    /// of the one carried (`null`), on this app's connection: `{answer}`, or `{refused}`. The
    /// first carry joins the call as the app, which the workspace's grants decide.
    #[napi]
    pub async fn carry_call(&self, pair: serde_json::Value) -> serde_json::Value {
        let calls = Arc::clone(&self.calls);
        match calls.carry_call(pair).await {
            Ok(answer) => serde_json::json!({ "answer": answer }),
            Err(refused) => serde_json::json!({ "refused": refused }),
        }
    }

    /// Say who is in the outside calls and where the app stands in them (`{calls, link,
    /// status}`): `{answer}`, or `{refused}`.
    #[napi]
    pub async fn report_call(&self, report: serde_json::Value) -> serde_json::Value {
        let calls = Arc::clone(&self.calls);
        match calls.report_call(report).await {
            Ok(answer) => serde_json::json!({ "answer": answer }),
            Err(refused) => serde_json::json!({ "refused": refused }),
        }
    }

    /// Hand the workspace's call one Opus frame `speaker`, an outside account, said: `null`, or
    /// `{refused}`. Nothing waits for an answer, as nothing waits in a live call.
    #[napi]
    pub fn hear_call(&self, speaker: String, opus: Buffer) -> serde_json::Value {
        match self.calls.hear_call(speaker, opus.into()) {
            Ok(()) => serde_json::Value::Null,
            Err(refused) => serde_json::json!({ "refused": refused }),
        }
    }

    /// What the workspace's call said since the last time, and where the device asks the app to
    /// be, when that is new. Throws the refusal's body, as JSON, on a connection that is no app's.
    #[napi]
    pub fn said_call(&self) -> napi::Result<CallSaid> {
        let said = self
            .calls
            .said_call()
            .map_err(|refused| napi::Error::from_reason(refused.to_string()))?;
        Ok(CallSaid {
            frames: said
                .frames
                .into_iter()
                .map(|(speaker, opus)| CallFrame {
                    speaker,
                    opus: opus.into(),
                })
                .collect(),
            asked: said.asked.map(|(call, status)| CallAsk { call, status }),
        })
    }
}

/// One Opus frame of a call, and the account that said it.
#[napi(object)]
pub struct CallFrame {
    pub speaker: String,
    pub opus: Buffer,
}

/// Where the device asks an app to be in the outside call it carries (`call: null`: in none), and
/// the line to show there about who is on the workspace's side.
#[napi(object)]
pub struct CallAsk {
    pub call: Option<String>,
    pub status: Option<String>,
}

/// What a carried call said since the app last looked.
#[napi(object)]
pub struct CallSaid {
    pub frames: Vec<CallFrame>,
    pub asked: Option<CallAsk>,
}
