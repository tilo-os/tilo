//! Starting an app from a path: `notes#open` starts an installed app and says what is in it,
//! and `pdf-viewer#open content=…` opens a file the workspace holds in the app that opens it,
//! whether or not the app runs already.

use super::{ActionHost, Addressed, McpServer, OneCall, Value, json};
use super::{Done, LsArgs, ParsedPath, Reply, app_names, outcome, starts_app};
use std::fmt::Write as _;

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// `notes#open` (or `notes#start`, without a window): start an installed app, then say
    /// what is in it.
    pub(super) async fn launch(
        &self,
        parsed: &ParsedPath,
        app_id: &str,
        action: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<(Value, Reply), String> {
        if !parsed.segments.is_empty() || !starts_app(action) {
            return Err(Self::not_running_note(&parsed.app, true));
        }
        let value = self
            .app_in_workspace(app_id, action, arguments, invocation, wait_ms)
            .await?;
        if outcome(&value).failed {
            let reply = outcome(&value);
            return Ok((value, reply));
        }
        let reply = self.opened_reply(app_id, &value).await;
        Ok((value, reply))
    }

    /// [`Self::launch`] as a call's answer: `pdf-viewer#open content=…` opens a file, running or not.
    pub(super) async fn launched(
        &self,
        (parsed, app_id, action): (&ParsedPath, &str, &str),
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Done, super::Failure> {
        let (value, reply) =
            Box::pin(self.launch(parsed, app_id, action, arguments, invocation, wait_ms)).await?;
        Ok(Done::Launched { value, reply })
    }

    /// `action` (`open` or `start`) of the app `resolved` names, which runs already: it is there,
    /// and what is in it. Running is not shown: `open` asks for the app's window on the person's
    /// shell, or brings the one there into view.
    pub(super) async fn running(&self, resolved: &super::Resolved, action: &str) -> Done {
        let app_id = resolved.app_id.as_str();
        if action == tilo_actions::remote::APPLICATION_OPEN_ACTION {
            self.show(&resolved.provider, app_id).await;
        }
        // What is in the window it answered with: the record the workspace lists
        // it on, so a caller opening an app that runs learns the window it
        // attached to and the record that window shows, as the open that made
        // it said.
        let record = self
            .providers()
            .await
            .ok()
            .and_then(|(_, listed)| {
                listed
                    .into_iter()
                    .find(|(provider, _)| *provider == resolved.provider)
                    .map(|(_, entry)| entry)
            })
            .and_then(|entry| {
                entry
                    .get("record_id")
                    .and_then(Value::as_str)
                    .filter(|record| !record.is_empty())
                    .map(str::to_owned)
            });
        let value = match record {
            Some(record) => json!({"update": {
                "state": "succeeded",
                "output": {"value": {
                    "provider": format!("{}:{app_id}:{record}", tilo_actions::remote::RECORD_PROVIDER),
                    "record": record,
                }},
            }}),
            None => json!({"update": {"state": "succeeded", "output": {}}}),
        };
        let reply = self.opened_reply(app_id, &value).await;
        Done::Launched { value, reply }
    }

    /// What starting `app_id` answers once it runs: that it does, and what
    /// is in it.
    pub(super) async fn opened_reply(&self, app_id: &str, value: &Value) -> Reply {
        let mut reply = outcome(value);
        let name = app_names(&[app_id.to_owned()]).join("");
        reply.text = format!("ok: {name} is running");
        // A file opened in it: the record it is in, as a path addresses it.
        let opened =
            (value.pointer("/update/output/value")).filter(|it| it.get("action").is_some());
        if let Some(provider) = opened.and_then(|opened| opened["provider"].as_str()) {
            reply.text = format!("ok: opened in {name}@{provider}");
        }
        if let Some(error) = value.pointer("/opened/error").and_then(Value::as_str) {
            let _ = write!(reply.text, "\nnot opened: {error}");
        }
        if let Ok(listing) = self
            .agent_ls(
                LsArgs {
                    path: Some(name.clone()),
                    ..LsArgs::default()
                },
                false,
            )
            .await
        {
            // An app that opened on the person's shell but publishes nothing an
            // agent lists (yet) is not answered as both running and not.
            if listing
                .text
                .starts_with(&Self::not_running_note(&name, true))
            {
                if reply.text.ends_with("is running") {
                    reply.text = format!(
                        "ok: {name} opened on the person's shell; it lists nothing to agents"
                    );
                }
                return reply;
            }
            reply.text.push('\n');
            reply.text.push_str(&listing.text);
        }
        reply
    }

    /// The workspace's `action` (its `open` or its `start`) of the app `app_id`, with
    /// `arguments` for the files it opens there: the workspace's answer, which names the
    /// instance it started at [`super::OPENED_PEER`].
    pub(super) async fn app_in_workspace(
        &self,
        app_id: &str,
        action: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Value, String> {
        Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: tilo_actions::remote::workspace_provider(&self.workspace),
                app_id: None,
            },
            workspace: true,
            revision: None,
            resource: format!(
                "{}{app_id}",
                tilo_actions::remote::APPLICATION_RESOURCE_PREFIX
            ),
            action: action.to_owned(),
            arguments,
            invocation_id: invocation,
            wait_ms,
        }))
        .await
        .map_err(String::from)
    }
}
