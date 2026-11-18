//! Files' reads of the folder of a workspace whose tree is kept here:
//! `app.so.tilo.file-explorer#list` and `#read-text`, each by a path in the workspace
//! (`Tasks/12 Plan/notes.md`).
//!
//! On a device a folder is listed by a window's rows and a file is read from that device's
//! disk. A caller with no window and no mount (an agent working while every device of its
//! person is off) asks the workspace instead, and whoever keeps its tree answers: what a folder
//! holds, a page at a time, and what a file says, put together from the content kept here of
//! it ([`fs_content::whole`]).
//!
//! Nothing is written, so nothing is taken back. A read is answered to whoever may read what
//! Files keeps in the workspace ([`Worker::may_on_tree`]).

use serde_json::{Map, Value, json};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{ActionCallError, ActorPlace, Caller, InvokeRequest};
use tilo_fs::namespace::Namespace;
use tilo_fs::tree_put::{FileAt, MOST_LISTED};
use tilo_server_protocol::WorkspaceId;

use super::refused;
use crate::workspace_actions::{Outcome, Worker, invoke_arguments};

/// The most of one file a text read takes, as a search of the files reads one for its words.
const MOST_TEXT_BYTES: u64 = 1024 * 1024;

/// The most characters one page of a file's text holds, as Files on a device pages one.
const PAGE_CHARS: usize = 64 * 1024;

/// The whole number `name` gives, `None` when it gives none.
fn place(arguments: &Map<String, Value>, name: &str) -> Result<Option<usize>, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|place| usize::try_from(place).ok())
            .map(Some)
            .ok_or_else(|| format!("`{name}` is a whole number, 0 or more")),
    }
}

/// How an answer names what an entry is.
const fn kind(folder: bool) -> &'static str {
    if folder { "folder" } else { "file" }
}

/// What reads a file that is no text, by what its name ends with: as Files on a device says it
/// of a document or a workbook.
fn read_by(path: &str) -> &'static str {
    let extension = path
        .rsplit_once('.')
        .map_or("", |(_, extension)| extension)
        .to_ascii_lowercase();
    match extension.as_str() {
        "docx" | "odt" | "rtf" | "epub" => "Writer's `read-file` reads it as Markdown",
        "xlsx" | "ods" => "Sheets' `read-file` reads its cells",
        "pdf" => "the PDF viewer's `read-text` reads it, once it is open there",
        _ => "the app that opens it reads it",
    }
}

/// What `list` answers: the entries of the folder `folder` names (the workspace's own when it
/// names none), by name, each with its path, whether it is a folder, a file's size and when
/// it last changed; or, with `trash`, what the trash holds, each with the id that brings it
/// back. A page at a time: `from` is where it starts, `next` where the page after it does.
fn listing(tree: &mut Namespace, arguments: &Map<String, Value>) -> Result<Value, String> {
    let from = place(arguments, "from")?.unwrap_or(0);
    let limit = place(arguments, "limit")?.unwrap_or(MOST_LISTED);
    match arguments.get("trash") {
        None | Some(Value::Null | Value::Bool(false)) => {}
        Some(Value::Bool(true)) => {
            let page = tree.list_trash(from, limit);
            let entries: Vec<Value> = page
                .entries
                .iter()
                .map(|entry| {
                    json!({
                        "id": entry.id,
                        "name": entry.name,
                        "was": entry.was,
                        "kind": kind(entry.folder),
                    })
                })
                .collect();
            return Ok(json!({
                "trash": true,
                "entries": entries,
                "from": page.from,
                "next": page.next,
                "total": page.total,
            }));
        }
        Some(_) => return Err("`trash` is true or false".to_owned()),
    }
    let folder = match arguments.get("folder") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(folder)) => folder.trim().to_owned(),
        Some(_) => return Err("`folder` is a path in the workspace, as text".to_owned()),
    };
    let page = tree.list_folder(&folder, from, limit)?;
    let entries: Vec<Value> = page
        .entries
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "name": entry.name,
                "path": entry.path,
                "kind": kind(entry.folder),
                "size": (!entry.folder).then_some(entry.size),
                "changed": entry.changed,
            })
        })
        .collect();
    Ok(json!({
        "folder": folder.trim_matches('/'),
        "entries": entries,
        "from": page.from,
        "next": page.next,
        "total": page.total,
    }))
}

impl Worker {
    /// The bytes of `file`: put together from the content the workspace's store keeps of it
    /// here.
    fn bytes_at(&self, workspace_id: WorkspaceId, file: &FileAt) -> Result<Vec<u8>, String> {
        let Some(content) = &file.content else {
            return Ok(Vec::new());
        };
        if file.size > MOST_TEXT_BYTES {
            return Err(format!(
                "{:?} is {} bytes, and a text read takes at most {MOST_TEXT_BYTES}: {}",
                file.path,
                file.size,
                read_by(&file.path)
            ));
        }
        let store = self
            .workspace_content(workspace_id)
            .ok_or("this workspace keeps no files where its tree is kept")?;
        let missing = || {
            format!(
                "the content of {:?} has not arrived where the workspace's tree is kept",
                file.path
            )
        };
        let manifest = store.read(content).ok_or_else(missing)?;
        fs_content::whole(manifest, MOST_TEXT_BYTES, |chunk| {
            store.read(&hex::encode(chunk))
        })
        .ok_or_else(missing)
    }

    /// What `read-text` answers: what the file `file` names says, by its path in the
    /// workspace, a page of characters at a time (`from`, `next`, `total`).
    fn text_at(
        &self,
        workspace_id: WorkspaceId,
        tree: &mut Namespace,
        arguments: &Map<String, Value>,
    ) -> Result<Value, String> {
        let named = arguments
            .get("file")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|file| !file.is_empty())
            .ok_or("`file` names the file by its path in the workspace")?;
        let from = place(arguments, "from")?.unwrap_or(0);
        let file = tree.file_at(named)?;
        let text = String::from_utf8(self.bytes_at(workspace_id, &file)?)
            .map_err(|_| format!("{:?} is not text: {}", file.path, read_by(&file.path)))?;
        let total = text.chars().count();
        let from = from.min(total);
        let to = from.saturating_add(PAGE_CHARS).min(total);
        let part: String = text
            .chars()
            .skip(from)
            .take(to.saturating_sub(from))
            .collect();
        Ok(json!({
            "file": file.path,
            "id": file.id,
            "text": part,
            "from": from,
            "next": (to < total).then_some(to),
            "total": total,
            "size": file.size,
            "changed": file.changed,
        }))
    }

    /// Answer one of Files' reads of the folder of the workspace `named`
    /// ([`fs_protocol::tree::is_tree_read`]): what a folder holds, or what a file says.
    ///
    /// # Errors
    /// A caller who may not read what Files keeps in the workspace, a call naming a device
    /// (whose own disk only that device reads), a path that is none of the workspace, a folder
    /// or file that is not there, a file that is no text or longer than a text read takes, or
    /// a tree this holder cannot read yet.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the holder keeps"
    )]
    pub(in crate::workspace_actions) fn read_tree(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        named: &str,
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        if !self.may_on_tree(workspace_id, caller, &[]) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let mut arguments = invoke_arguments(&request.argument);
        // A file a listing offered names itself: its read needs no path besides.
        if let Some(path) = fs_protocol::tree::file_of_resource(&request.target.address.resource) {
            arguments.insert("file".to_owned(), Value::String(path.to_owned()));
        }
        if arguments.contains_key("device") {
            return Err(refused(
                "a path in the workspace names no device: a device's own disk is read by Files \
                 on that device",
            ));
        }
        let (mut tree, _) = self.tree_in_workspace(workspace_id, named)?;
        let value = if request.target.address.action == fs_protocol::tree::LIST {
            listing(&mut tree, &arguments)
        } else {
            self.text_at(workspace_id, &mut tree, &arguments)
        }
        .map_err(refused)?;
        self.keep_tree(workspace_id, named, tree);
        Ok((
            ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
            None,
        ))
    }
}
