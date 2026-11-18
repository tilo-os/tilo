//! What a view asks of the workspace it is shown in: the records its apps keep, and the actions
//! it is allowed. Both go through the session's action API ([`Session`]), as the person at the
//! window, so the workspace's own grants decide every read and every change.

use serde_json::{Map, Value as Json};
use tilo_actions::remote::{
    ActionCallError, InvokeArgument, ResourceReadRequest, application_action_target,
};
use ui::arriving::Arrival;
use ui::remote_actions::{InvocationCall, ResourceReadCall};

/// How long a read asked again waits for the record to change before it answers as it stands.
/// The host caps it; the view asks again either way.
const WAIT_MS: u32 = 10_000;

/// What came of reading an app's record of the workspace.
pub(crate) enum Read {
    /// The record as the app stores it, and the revision it stands at.
    Record { revision: String, value: Json },
    /// The app keeps no record here yet, or it has not reached this device.
    Absent { revision: String },
    /// The person may not read it.
    Refused,
    /// It could not be read here for another reason.
    Unread(String),
}

pub(crate) trait Workspace {
    /// Ask for the record the app `app` keeps for the workspace: as it stands, or with `after`
    /// once it differs from that revision.
    /// `document` names one document of the app's own to read in place of that one; empty, it is
    /// the one above.
    fn read(&mut self, app: &str, document: &str, after: Option<String>);
    /// The reads answered since the last call, each with the app and the document it was asked of.
    fn take_read(&mut self) -> Vec<(String, String, Read)>;
    /// Invoke `verb` of the app `app` on its resource `resource`, as the person at the window.
    fn invoke(&mut self, app: &str, resource: &str, verb: &str, arguments: Map<String, Json>);
    /// Why each invocation that did not do what was asked did not, since the last call.
    fn take_failed(&mut self) -> Vec<String>;
}

/// The workspace through the action API of the window that shows the view.
pub(crate) struct Session {
    api: ui::RemoteActions,
    reads: Vec<(String, String, ResourceReadCall)>,
    calls: Vec<InvocationCall>,
    failed: Vec<String>,
}

impl Session {
    pub(crate) fn new(api: ui::RemoteActions) -> Self {
        Self {
            api,
            reads: Vec::new(),
            calls: Vec::new(),
            failed: Vec::new(),
        }
    }
}

impl Workspace for Session {
    fn read(&mut self, app: &str, document: &str, after: Option<String>) {
        let read = ResourceReadRequest {
            app: app.to_owned(),
            record: document.to_owned(),
            joined: false,
            wait_ms: after.is_some().then_some(WAIT_MS),
            after,
        };
        self.reads.push((
            app.to_owned(),
            document.to_owned(),
            self.api.read_resource(None, read),
        ));
    }

    fn take_read(&mut self) -> Vec<(String, String, Read)> {
        let mut done = Vec::new();
        self.reads.retain(|(app, document, call)| {
            let read = call.with(|arrival| match arrival {
                Arrival::Pending { .. } => None,
                Arrival::Ready(answer) => Some(answered(answer)),
                Arrival::Failed(error) => Some(unanswered(error)),
            });
            match read {
                Some(read) => {
                    done.push((app.clone(), document.clone(), read));
                    false
                }
                None => true,
            }
        });
        done
    }

    fn invoke(&mut self, app: &str, resource: &str, verb: &str, arguments: Map<String, Json>) {
        match ui::framework::reference::workspace() {
            Ok(workspace) => {
                let target = application_action_target(&workspace, app, resource, verb);
                self.calls.push(
                    self.api
                        .invoke(target, InvokeArgument::Json(arguments), None),
                );
            }
            Err(why) => self.failed.push(why),
        }
    }

    fn take_failed(&mut self) -> Vec<String> {
        let mut failed = std::mem::take(&mut self.failed);
        self.calls.retain(|call| {
            call.with(|arrival| match arrival {
                Arrival::Pending { .. } => true,
                Arrival::Ready(update) => !update.is_terminal(),
                Arrival::Failed(error) => {
                    failed.push(error.to_string());
                    false
                }
            })
        });
        failed
    }
}

/// What a record read answered: `{"revision", "records": [{"record", "value" | "absent" |
/// "arriving"}]}`, of which a view asks for one record.
fn answered(answer: &Json) -> Read {
    let revision = answer
        .get("revision")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned();
    let value = answer
        .get("records")
        .and_then(|records| records.get(0))
        .and_then(|record| record.get("value"));
    match value {
        Some(value) => Read::Record {
            revision,
            value: value.clone(),
        },
        None => Read::Absent { revision },
    }
}

fn unanswered(error: &ActionCallError) -> Read {
    match error {
        ActionCallError::Refused { .. } | ActionCallError::NotPermitted => Read::Refused,
        other => Read::Unread(other.to_string()),
    }
}
