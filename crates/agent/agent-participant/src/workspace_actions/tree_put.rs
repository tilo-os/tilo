//! Putting a file into a workspace's file tree: `app.so.tilo.file-explorer#open`
//! given a file's `content`, and the folder (`dir`) and name it goes under.
//!
//! A workspace has one folder — its Files app record, the tree its members
//! mount — and a file gets into it by being written there. A caller that has
//! the bytes but no mount (the `tilo` command on a machine holding only a key,
//! an agent over MCP) puts the bytes in the workspace's store and opens them
//! in Files; this host writes the node where the filesystem writes one it
//! creates ([`tilo_fs::namespace`]), so every member's folder shows the file
//! as if it had been copied in there. No app's record is made: what opens the
//! file afterwards is the person's choice, from the file itself.

use collab_types::DocumentMutation;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{ActionHostError, Caller, InvokeRequest};
use tilo_fs::namespace::Namespace;
use tilo_fs::tree_record::{FILES_APP, record as tree_record};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::WorkspaceId;

use super::{Needs, Outcome, Worker, commit_failed, invoke_arguments};

/// What a `files` open names of the file it puts into the workspace.
pub(super) struct Putting {
    /// Its `tilo.content:<hash>` reference.
    pub(super) content: String,
    /// The hash the reference names.
    pub(super) hash: String,
    /// The folder in the tree it goes into, `""` for the workspace's root.
    pub(super) dir: String,
    /// The name it takes in that folder.
    pub(super) name: String,
}

fn refused(reason: impl Into<String>) -> Outcome {
    Outcome::Rejected(ActionHostError::Refused {
        reason: reason.into(),
    })
}

/// A folder path as the tree keeps it: no empty, leading or trailing parts,
/// and no part a folder may not be called.
fn folder(path: &str) -> Result<String, String> {
    let parts: Vec<&str> = path
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    for part in &parts {
        if *part == "." || *part == ".." || part.contains('\0') {
            return Err(format!("{part:?} is no folder's name"));
        }
    }
    Ok(parts.join("/"))
}

impl Putting {
    /// What `arguments` name of the file to put into the workspace.
    ///
    /// # Errors
    /// No content reference, a name a file may not have, or a folder path
    /// that is no path.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn named(arguments: &Map<String, Value>) -> Result<Self, Outcome> {
        let content = arguments
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
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
            .unwrap_or_default()
            .to_owned();
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
            return Err(refused(format!("{name:?} is no file's name")));
        }
        let dir = folder(
            arguments
                .get("dir")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
        .map_err(refused)?;
        Ok(Self {
            content,
            hash,
            dir,
            name,
        })
    }
}

impl Worker {
    /// The record of `workspace`'s files.
    fn tree_route(workspace: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: FILES_APP.to_owned(),
            cluster_id: workspace.to_owned(),
            record: tree_record(),
        }
    }

    /// Answer a Files open naming a file: put it into `named`'s tree at the
    /// folder and under the name it asks for, founding the tree's record when
    /// the workspace holds none, and answer where it now is.
    ///
    /// # Errors
    /// A caller who may not change the workspace's records, content the
    /// workspace does not hold, a tree the host cannot read yet, or a change
    /// the workspace did not take.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn put_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        named: &str,
    ) -> Result<
        (
            tilo_actions::ActionOutput,
            Option<tilo_actions::remote::ActorPlace>,
        ),
        Outcome,
    > {
        use tilo_actions::ActionOutput;
        if !self.may_across(
            workspace_id,
            caller,
            ui::permissions::policy::ActionKind::Undoable,
        ) {
            return Err(Outcome::Rejected(ActionHostError::NotPermitted));
        }
        let putting = Putting::named(&invoke_arguments(&request.argument))?;
        let store = self
            .workspace_content(workspace_id)
            .ok_or_else(|| refused("this workspace keeps no files on its host"))?;
        // Sealed content keeps its length only in its seal: it is read whole.
        let size = match store.read_range(&putting.hash, 0, 1) {
            Ok((size, _)) => size,
            Err(_) => match store.read(&putting.hash) {
                Some(bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                None => {
                    return Err(refused(format!(
                        "this workspace holds no {}",
                        putting.content
                    )));
                }
            },
        };
        let record = tree_record();
        let route = Self::tree_route(named);
        // The tree of a governed workspace is sealed: its keys are what
        // reading it and writing to it need.
        let held = self
            .refresh(
                workspace_id,
                Needs::Act {
                    app_id: FILES_APP,
                    record: &record,
                },
            )
            .map_err(Outcome::Rejected)?;
        let existing = match held.document(&route) {
            Ok((bytes, _)) => Some(bytes),
            Err(_) if !self.held_or_owed(workspace_id, &route)? => None,
            Err(why) => return Err(refused(why)),
        };
        let author = hex::encode(self.host_device.key.verifying_key().to_bytes());
        let mut tree = match &existing {
            Some(bytes) => Namespace::from_bytes(&author, bytes)
                .map_err(|error| refused(format!("the workspace's tree does not read: {error}")))?,
            None => Namespace::new(&author)
                .map_err(|error| refused(format!("a tree could not be founded: {error}")))?,
        };
        let mtime = tilo_clock::unix_now_ns() / 1_000_000;
        let put = tree
            .put_file(&putting.dir, &putting.name, &putting.hash, size, mtime)
            .map_err(refused)?;
        let mutation = if existing.is_some() {
            DocumentMutation::Incremental(put.change)
        } else {
            DocumentMutation::Snapshot(tree.save())
        };
        self.commit(workspace_id, &route, &mutation)
            .map_err(|reason| commit_failed(reason, true))?;
        Ok((
            ActionOutput {
                value: Some(json!({
                    "path": put.path,
                    "content": putting.content,
                    "size": size,
                    "replaced": put.replaced,
                })),
                resources: Vec::new(),
            },
            None,
        ))
    }
}
