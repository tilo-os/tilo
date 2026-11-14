//! Opening an app on a file the workspace holds: `app.<app>#open` given the
//! file's `content` (and its `name`), as a desktop opens a file in the app
//! that opens its type.
//!
//! An app that keeps one document per record (PDF Viewer, Writer, Slides,
//! Sheets) opens a record of its own for the file, so a folder of PDFs is a
//! PDF each, never one PDF and the rest refused. An app that keeps many files
//! in one record (a notebook, a photo library) takes it into the record the
//! workspace keeps of it. Either way the file goes in through the app's own
//! action for it ([`tilo_content_types::landing_in`]), run on that record
//! as any caller's action is, in the same invocation as the open: the API,
//! the `tilo` command, an agent and a person's page all open a file this one
//! way.

use serde_json::{Map, Value, json};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{
    ActionHostError, ActorPlace, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_content_types::{Landing, Takes};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::WorkspaceId;

use super::{Outcome, Worker, WorkspaceState, record_provider};

/// The most of a text file a note takes: a note is words, not an archive.
const MAX_NOTE_BYTES: u64 = 8 * 1024 * 1024;

/// The file an `open` names: its reference and the name it goes by.
pub(super) struct OpenedFile {
    content: String,
    hash: String,
    name: String,
}

impl OpenedFile {
    /// The file `arguments` name, `None` for an open that names none (an
    /// empty record), or why the file they name is not one.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn named(arguments: &Map<String, Value>) -> Result<Option<Self>, Outcome> {
        let Some(content) = arguments.get("content") else {
            return Ok(None);
        };
        let content = content.as_str().unwrap_or_default().trim().to_owned();
        let hash = tilo_actions::content_hash(&content)
            .ok_or_else(|| {
                refused(format!(
                    "`{content}` names no content: give the file's `tilo.content:<reference>`"
                ))
            })?
            .to_owned();
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Untitled")
            .to_owned();
        Ok(Some(Self {
            content,
            hash,
            name,
        }))
    }
}

fn refused(reason: String) -> Outcome {
    Outcome::Rejected(ActionHostError::Refused { reason })
}

impl Worker {
    /// How `file` lands in `app`: the app's action for a file, refused
    /// before any record is made when the app does not open what the file's
    /// bytes are.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn landing_of(
        &self,
        workspace_id: WorkspaceId,
        app: &str,
        file: &OpenedFile,
    ) -> Result<(Landing, Map<String, Value>), Outcome> {
        let store = self.workspace_content(workspace_id).ok_or_else(|| {
            refused("this workspace keeps no files on its host to open".to_owned())
        })?;
        let missing = || refused(format!("this workspace holds no {}", file.content));
        let head_bytes = u64::try_from(tilo_content_types::HEAD_BYTES).unwrap_or(u64::MAX);
        let head = match store.read_range(&file.hash, 0, head_bytes) {
            Ok((_, head)) => head,
            // Sealed content keeps its length in its seal: it is read whole.
            Err(_) => store.read(&file.hash).ok_or_else(missing)?,
        };
        let landed = tilo_content_types::landing(&head, &file.name);
        let landing = landed
            .filter(|landing| tilo_content_types::landing_in(app) == Some(*landing))
            .ok_or_else(|| {
                let (extension, _) = tilo_content_types::opened_as(&head);
                let opens = landed.map_or_else(
                    || "no app here opens it".to_owned(),
                    |landing| format!("{} opens it", landing.app),
                );
                refused(format!(
                    "{} is a {extension} file, which {app} does not open: {opens}",
                    file.name
                ))
            })?;
        let mut arguments = Map::new();
        match landing.takes {
            Takes::ContentNamed => {
                arguments.insert("content".into(), json!(file.content));
                arguments.insert("name".into(), json!(file.name));
            }
            Takes::ContentTitled => {
                arguments.insert("content".into(), json!(file.content));
                arguments.insert("title".into(), json!(file.name));
            }
            Takes::Content => {
                arguments.insert("content".into(), json!(file.content));
            }
            Takes::Text => {
                // Its size, from a read of its first byte; sealed content
                // states none that way and is read whole below.
                let size = store
                    .read_range(&file.hash, 0, 1)
                    .map_or(0, |(size, _)| size);
                if size > MAX_NOTE_BYTES {
                    return Err(refused(format!(
                        "{} is too long for a note: keep it as a file",
                        file.name
                    )));
                }
                let bytes = store.read(&file.hash).ok_or_else(missing)?;
                let body = String::from_utf8(bytes)
                    .map_err(|_| refused(format!("{} is not text", file.name)))?;
                arguments.insert("title".into(), json!(file.name));
                arguments.insert("body".into(), json!(body));
            }
            Takes::Image => {
                let (_, width, height) =
                    tilo_content_types::image_size(&head).ok_or_else(|| {
                        refused(format!("{} has no size a picture states", file.name))
                    })?;
                arguments.insert(
                    "image".into(),
                    json!({"name": file.name, "width": width, "height": height, "content": file.content}),
                );
            }
        }
        Ok((landing, arguments))
    }

    /// Answer the workspace's `app.<app>#open` (named `named`) in `request`:
    /// a new record of the app, or, given a file, the record the file
    /// lands in with the file in it.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn open_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        (named, state): (&str, &WorkspaceState),
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let target = &request.target;
        // How a file lands is checked before any record is made for it.
        let file = OpenedFile::named(&super::invoke_arguments(&request.argument))?;
        let landing = match &file {
            Some(file) => {
                let app = self.asked_app(target).ok_or_else(super::unknown_action)?;
                Some(self.landing_of(workspace_id, &app, file)?)
            }
            None => None,
        };
        let kept = landing
            .as_ref()
            .filter(|(landing, _)| !landing.own_record)
            .and_then(|_| self.asked_app(target))
            .and_then(|app| Self::kept_record(state, &app));
        let opened = match kept {
            Some(route) => Self::opened_answer(&route),
            None => self.open_asked(workspace_id, caller, named, target)?,
        };
        match landing {
            Some((landing, arguments)) => {
                self.open_file_in(workspace_id, caller, request, (opened, landing, arguments))
            }
            None => Ok((opened, None)),
        }
    }

    /// The record of `app` a file that lands in a record the app already
    /// keeps goes into: the one it shows or changed last, `None` for none.
    pub(super) fn kept_record(state: &WorkspaceState, app: &str) -> Option<DocumentRoute> {
        state
            .records
            .iter()
            .find(|route| route.app_id == app)
            .cloned()
    }

    /// Run `landing`'s action with `arguments` on the record `opened`
    /// answered, as the next step of `request`, and answer both: the record,
    /// and what the app said of the file.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn open_file_in(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        (opened, landing, arguments): (ActionOutput, Landing, Map<String, Value>),
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let provider = opened
            .value
            .as_ref()
            .and_then(|value| value.get("provider"))
            .and_then(Value::as_str)
            .ok_or_else(|| refused("the app opened no record for the file".to_owned()))?
            .to_owned();
        let import = InvokeRequest {
            target: RemoteActionTarget {
                provider,
                address: tilo_actions::ActionAddress::new("", landing.action),
            },
            argument: InvokeArgument::Json(arguments),
            revision: None,
            confirmation: None,
            appearance: None,
            ..request.clone()
        };
        let (output, left_at) = self.perform(workspace_id, caller, &import)?;
        let mut value = opened.value.unwrap_or_else(|| json!({}));
        if let Some(fields) = value.as_object_mut() {
            fields.insert("action".into(), json!(landing.action));
            fields.insert("opened".into(), output.value.unwrap_or(Value::Null));
        }
        Ok((
            ActionOutput {
                value: Some(value),
                resources: output.resources,
            },
            left_at,
        ))
    }

    /// The answer `open` gives for the record `route` it opened.
    pub(super) fn opened_answer(route: &DocumentRoute) -> ActionOutput {
        ActionOutput {
            value: Some(json!({
                "provider": record_provider(route),
                "record": route.record,
            })),
            resources: Vec::new(),
        }
    }
}
