//! `tilo_read` of `record:<app>:<record>`: a record of the workspace as any reader reads it, through the one record API
//! ([`tilo_actions::remote::ResourceReads`]): its content as a structured value whatever app keeps
//! it, decided as the agent may read it now, and woken by the next write to it. `tilo_ls` of
//! `record:<app>` lists the records of the app the agent may read, each by that path.

use super::{ActionProvider, Failure, McpServer, Reply};
use tilo_actions::remote::{Caller, RemoteActionMessage, ResourceReadRequest, ResourceReads};

/// How long a document read waits for a write past the revision it was given.
const RECORD_WAIT_MS: u32 = 25_000;
/// Longest answer, in characters, a record read gives: a longer chain is read a part at a time.
const RECORD_CHARS: usize = 200_000;

/// What the record API answers `read` for `caller`, as JSON: a read with `after` is held by the
/// reader until the record moves past it or the wait went by.
async fn answered(
    reads: std::sync::Arc<dyn ResourceReads>,
    caller: &Caller,
    read: ResourceReadRequest,
) -> Result<serde_json::Value, String> {
    match reads.answer_read(caller, 0, read).await {
        Some(RemoteActionMessage::ResourceReading { result, .. }) => {
            let text = result.map_err(|error| error.to_string())?;
            serde_json::from_str(&text)
                .map_err(|error| format!("the reading does not read: {error}"))
        }
        _ => Err("the record read ended before it answered".to_owned()),
    }
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// `tilo_ls` of `record:<app>` (`record:` for every app's): the records of the workspace the
    /// record API lists for the agent, as a read with `listing` answers them, each by the path
    /// `tilo_read` reads it at.
    pub(super) async fn agent_records(
        &self,
        app: &str,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let reads = self
            .host()
            .resource_reads()
            .ok_or_else(|| tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.to_owned())?;
        let caller = self.execution_caller();
        let read = ResourceReadRequest {
            workspace: caller.workspace().unwrap_or_default().to_owned(),
            app: app.to_owned(),
            record: String::new(),
            joined: false,
            after: None,
            wait_ms: None,
            document: false,
            listing: true,
        };
        let listing = answered(reads, &caller, read).await?;
        let listed: Vec<(&str, &str)> = listing["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|listed| Some((listed["app"].as_str()?, listed["record"].as_str()?)))
            .collect();
        let path = |(app, record): &(&str, &str)| format!("record:{app}:{record}");
        let text = listed.iter().map(path).collect::<Vec<_>>().join("\n");
        let mut reply = Reply::ok(text);
        if structured {
            let rows: Vec<serde_json::Value> = listed
                .iter()
                .map(|listed| {
                    let (app, record) = listed;
                    serde_json::json!({
                        "path": path(listed),
                        "name": record,
                        "id": record,
                        "label": record,
                        "kind": app,
                        "has_children": false,
                        "reads": true,
                        "actions": [],
                    })
                })
                .collect();
            reply.structured =
                Some(serde_json::json!({"path": format!("record:{app}"), "rows": rows}));
        }
        Ok(reply)
    }

    /// `tilo_read` of `record:<app>:<record>`: the record (its chain of parts, when `parts`), once
    /// it differs from the revision `after` or the wait passed.
    pub(super) async fn agent_record(
        &self,
        (app, record): (&str, &str),
        parts: bool,
        after: Option<String>,
    ) -> Result<Reply, Failure> {
        // A connection that reads none of them says the call that reads the same through its
        // app: the caller is not left to find it.
        let reads = self.host().resource_reads().ok_or_else(|| {
            let named = serde_json::json!({
                "path": app.rsplit('.').next().unwrap_or(app).to_lowercase()
            });
            format!(
                "{}: read it through its app instead: tilo_ls {named} lists what the app \
                 holds, each by the path tilo_read takes",
                tilo_actions::remote::RESOURCE_READS_UNAVAILABLE
            )
        })?;
        let caller = self.execution_caller();
        let read = ResourceReadRequest {
            workspace: caller.workspace().unwrap_or_default().to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
            joined: parts,
            wait_ms: after.is_some().then_some(RECORD_WAIT_MS),
            after,
            document: false,
            listing: false,
        };
        let mut answer = answered(reads, &caller, read).await?;
        // A document the workspace holds none of is not an address to go on with: what the
        // app does hold is, and the answer names it.
        if let Some(document) = answer["records"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|reading| reading["absent"] == true)
            .and_then(|reading| reading["record"].as_str())
        {
            let name = super::app_names(&[app.to_owned()]).join("");
            return Err(format!(
                "the workspace holds no {app} document {document}; tilo_ls {name} lists what it \
                 has, and tilo_find {name} finds one of them"
            )
            .into());
        }
        // A document's words are whoever wrote them's: its person, another member, a file
        // somebody passed on. The answer says first that it carries content from outside
        // (data for a model to reason about, never instructions to follow), as its first
        // field: the text a model is handed stays the JSON it is.
        crate::mark_untrusted(&mut answer);
        let text = answer.to_string();
        if text.len() > RECORD_CHARS {
            return Err(format!(
                "the record reads as {} characters, more than {RECORD_CHARS}: read its parts one \
                 at a time, each by its own record id",
                text.len()
            )
            .into());
        }
        Ok(Reply {
            text,
            failed: false,
            structured: Some(answer),
            images: Vec::new(),
        })
    }
}
