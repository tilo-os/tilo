//! Files' reads of the folder of a workspace, answered where its tree is:
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
use tilo_fs::namespace::{FileAt, MOST_LISTED};
use tilo_server_protocol::WorkspaceId;

use super::refused;
use super::sealed::FileSealing;
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

/// Each document of `tree`, by its chunk, as a copy whose changes and their entries are read.
pub(super) fn chunk_documents(
    tree: &mut Namespace,
) -> Vec<(fs_protocol::files::TreeChunk, automerge::AutoCommit)> {
    tree.document_writes(&std::collections::BTreeMap::new())
        .into_iter()
        .filter_map(|write| {
            let mut doc = automerge::AutoCommit::new();
            tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &write.changes).ok()?;
            Some((write.chunk, doc))
        })
        .collect()
}

/// How many of the folder's changes [`history`] lists when it is not told.
const HISTORY_LAST: usize = 50;

/// The folder's changes, the newest `last` of them, oldest first: each as a document's History
/// lists a change of it ([`ui::history_source::verbs::entries_after`]), what it did, to which
/// `item`, by whom (`actor`, with the agent's `name` and the person it acted `for`) and when,
/// with the path in the workspace of the file it acted on. Its `change` is the `step` that
/// `undo` takes it back by. A change is kept in the document of one chunk of the tree, and the chunks' changes
/// are listed together, by their time.
fn history(tree: &mut Namespace, arguments: &Map<String, Value>) -> Result<Value, String> {
    let last = place(arguments, "last")?
        .unwrap_or(HISTORY_LAST)
        .clamp(1, ui::history_source::folder::LIMIT);
    let documents = chunk_documents(tree);
    let mut chunks = fs_protocol::tree::TreeChunks::default();
    for (chunk, doc) in &documents {
        if *chunk == 0 {
            chunks.read_index(doc);
        }
        chunks.read_chunk(*chunk, doc);
    }
    let named = chunks.tree();
    let words = &ui::history_source::sentence::ThreadWords;
    // A change a mounted folder wrote says no time: it is placed at the time of the change
    // before it in its document, so each document's changes keep the order they were made in.
    // The tree's own document (chunk 0) holds no file: what changed it, its founding and the
    // chunks it lists, was part of making a folder or a file, which the chunk lists.
    let mut listed: Vec<(i64, Value)> = Vec::new();
    for (chunk, doc) in documents.iter().filter(|(chunk, _)| *chunk != 0) {
        let mut before = 0;
        for mut entry in ui::history_source::verbs::entries_after(doc, &[], words) {
            let Some(fields) = entry.as_object_mut() else {
                continue;
            };
            let path = fields
                .get("item")
                .and_then(|item| item.get("id"))
                .and_then(Value::as_str)
                .filter(|id| named.node(id).is_some())
                .map(|id| fs_protocol::tree::path_of(&named, id));
            if let Some(path) = path {
                fields.insert("path".to_owned(), path.into());
            }
            fields.insert("chunk".to_owned(), (*chunk).into());
            let time = fields.get("time_ms").and_then(Value::as_i64).unwrap_or(0);
            before = before.max(time);
            listed.push((before, entry));
        }
    }
    listed.sort_by_key(|(time, _)| *time);
    let total = listed.len();
    let shown: Vec<Value> = listed
        .split_off(total.saturating_sub(last))
        .into_iter()
        .map(|(_, entry)| entry)
        .collect();
    Ok(json!({
        "versions": shown,
        "total": total,
        "undo": "`undo` with `step` set to a version's `change` takes that change back",
    }))
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

/// The page of `text` that starts at its character `from`: its characters, where it starts,
/// where the page after it does, and how many characters the text has.
fn page(text: &str, from: usize) -> (String, usize, Option<usize>, usize) {
    let total = text.chars().count();
    let from = from.min(total);
    let to = from.saturating_add(PAGE_CHARS).min(total);
    let part = text
        .chars()
        .skip(from)
        .take(to.saturating_sub(from))
        .collect();
    (part, from, (to < total).then_some(to), total)
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
    fn bytes_at(
        &self,
        workspace_id: WorkspaceId,
        sealing: Option<&FileSealing>,
        file: &FileAt,
    ) -> Result<Vec<u8>, String> {
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
        let sealed = fs_content::Manifest::decode(&manifest)
            .ok()
            .is_some_and(|manifest| manifest.sealed().is_some());
        if sealed && sealing.is_none() {
            return Err(
                "this workspace's files are sealed, and their keys are not here".to_owned(),
            );
        }
        fs_content::whole_with(
            manifest,
            MOST_TEXT_BYTES,
            |chunk| store.read(&hex::encode(chunk)),
            |cipher, bytes| sealing?.open(cipher, bytes),
        )
        .ok_or_else(missing)
    }

    /// What `read-text` answers: what the file `file` names says, by its path in the
    /// workspace, a page of characters at a time (`from`, `next`, `total`).
    fn text_at(
        &self,
        workspace_id: WorkspaceId,
        sealing: Option<&FileSealing>,
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
        let text = String::from_utf8(self.bytes_at(workspace_id, sealing, &file)?)
            .map_err(|_| format!("{:?} is not text: {}", file.path, read_by(&file.path)))?;
        let (part, from, next, total) = page(&text, from);
        Ok(json!({
            "file": file.path,
            "id": file.id,
            "text": part,
            "from": from,
            "next": next,
            "total": total,
            "size": file.size,
            "changed": file.changed,
        }))
    }

    /// What `read-text` answers for content the workspace keeps, named by its address
    /// (`content`, a `tilo.content:<hash>` reference, as a message that carries a file names
    /// it): what it says, a page of characters at a time, as a file of the folder is read.
    fn text_of_content(
        &self,
        workspace_id: WorkspaceId,
        arguments: &Map<String, Value>,
    ) -> Result<Value, String> {
        if arguments.contains_key("file") {
            return Err("`file` or `content` names what is read, not both".to_owned());
        }
        let address = arguments
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        let hash = tilo_actions::content_hash(address).ok_or_else(|| {
            format!("`content` is a `tilo.content:<hash>` address, and {address:?} is none")
        })?;
        let from = place(arguments, "from")?.unwrap_or(0);
        let store = self
            .workspace_content(workspace_id)
            .ok_or("this workspace keeps no files where its tree is kept")?;
        let most = MOST_TEXT_BYTES.saturating_add(1);
        let (size, bytes) = if let Ok((size, bytes)) = store.read_range(hash, 0, most) {
            (size, bytes)
        } else {
            // Sealed content keeps its length in its seal: it is read whole.
            let bytes = store
                .read(hash)
                .ok_or_else(|| format!("this workspace keeps no {address}"))?;
            (u64::try_from(bytes.len()).unwrap_or(u64::MAX), bytes)
        };
        if size > MOST_TEXT_BYTES {
            return Err(format!(
                "{address} is {size} bytes, and a text read takes at most {MOST_TEXT_BYTES}: \
                 the app that opens it reads it"
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{address} is not text: the app that opens it reads it"))?;
        let (part, from, next, total) = page(&text, from);
        Ok(json!({
            "content": address,
            "text": part,
            "from": from,
            "next": next,
            "total": total,
            "size": size,
        }))
    }

    /// Answer one of Files' reads of the folder of the workspace
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
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        if !self.may_on_tree(workspace_id, caller, &[]) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let mut arguments = invoke_arguments(&request.argument);
        // A file a listing offered names itself: its read needs no path besides.
        if let Some(path) = fs_protocol::tree::file_of_resource(&request.target.address.resource) {
            arguments.insert("file".to_owned(), Value::String(path.to_owned()));
        }
        // So does a folder a listing offered: what it holds is listed by it.
        if let Some(path) = fs_protocol::tree::folder_of_resource(&request.target.address.resource)
        {
            arguments.insert("folder".to_owned(), Value::String(path.to_owned()));
        }
        if arguments.contains_key("device") {
            return Err(refused(
                "a path in the workspace names no device: a device's own disk is read by Files \
                 on that device",
            ));
        }
        // Content the workspace keeps is read by its address, with no look at the tree: by
        // whoever may also fetch the workspace's content by its address, never by more.
        if request.target.address.action == fs_protocol::tree::READ_TEXT
            && arguments.contains_key("content")
        {
            if !self.may_across(
                workspace_id,
                caller,
                ui::permissions::policy::ActionKind::Read,
            ) {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
            let value = self
                .text_of_content(workspace_id, &arguments)
                .map_err(refused)?;
            return Ok((
                ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
                None,
            ));
        }
        let link = self
            .shared
            .link
            .get()
            .cloned()
            .ok_or_else(|| refused("this host keeps no workspace's tree"))?;
        let action = request.target.address.action.clone();
        // The tree's content keys are asked of the host before the tree is read: the read runs
        // on the tree's own thread, where the host is not asked again.
        let sealing = if action == fs_protocol::tree::READ_TEXT {
            self.file_sealing(workspace_id, false).map_err(refused)?
        } else {
            None
        };
        let value = link
            .read_tree(workspace_id, &mut |tree| {
                if action == fs_protocol::tree::LIST {
                    listing(tree, &arguments)
                } else if action == fs_protocol::tree::HISTORY {
                    history(tree, &arguments)
                } else {
                    self.text_at(workspace_id, sealing.as_ref(), tree, &arguments)
                }
            })
            .map_err(refused)?;
        Ok((
            ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
            None,
        ))
    }
}
