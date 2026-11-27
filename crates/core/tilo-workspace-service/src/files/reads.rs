//! Files' reads of a workspace's own folder, answered where this device holds its tree: `list`,
//! what a folder holds a page at a time, and `read-text`, what a file says.
//!
//! On a device a folder is listed by a window's rows and a file is read from the mount. A caller
//! with neither (an agent, or a key going through the workspace's listing) asks the workspace,
//! and the device that holds its tree answers: a folder by its path in the workspace
//! (`Tasks/12 Plan`), a file by its path or the content address a message names it by, its text
//! put together from the content kept here of it and opened with the workspace's Files keys
//! where they seal it.
//!
//! A content get of the caller's workspace ([`ContentGets`]) is answered here too: content put
//! into the workspace as it was given, and a file of its tree, named by its manifest, as the file
//! it describes, a piece at a time.
//!
//! Nothing is written, so nothing is taken back.

use std::ops::Range;

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, Caller, ContentGets, ContentPiece, MAX_CONTENT_FRAME_BYTES,
};
use tilo_fs::content::{Manifest, SealedChunks, is_manifest, whole_with};
use tilo_fs::namespace::{MOST_LISTED, Namespace};
use tilo_server_protocol::WorkspaceId;

use super::{OpenedFiles, files_scope};

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
/// names none), by name, each with its path, whether it is a folder, a file's size and when it
/// last changed; or, with `trash`, what the trash holds, each with the id that brings it back.
/// A page at a time: `from` is where it starts, `next` where the page after it does.
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

impl OpenedFiles {
    /// Answer Files' read `action` of `workspace`'s folder with `arguments`: `list` what a
    /// folder holds, `read-text` what a file says, the file named by its path (`file`) or by
    /// the content address a message names it by (`content`).
    ///
    /// # Errors
    /// Another action, a path that is none of the workspace, a folder or file that is not
    /// there, a file that is no text or longer than a text read takes, content that has not
    /// arrived on this device, keys the workspace seals its files with that are not here, or
    /// this device does not hold the workspace.
    pub fn read_folder(
        &self,
        workspace: WorkspaceId,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, String> {
        if arguments.contains_key("device") {
            return Err(
                "a path in the workspace names no device: a device's own disk is read by Files \
                 on that device"
                    .to_owned(),
            );
        }
        match action {
            fs_protocol::tree::LIST => tilo_rt::app()
                .block_on(
                    self.workspaces
                        .puts()
                        .read(workspace, &mut |tree| listing(tree, arguments)),
                )
                .map_err(|error| error.to_string())?,
            fs_protocol::tree::READ_TEXT if arguments.contains_key("content") => {
                self.text_of_content(workspace, arguments)
            }
            fs_protocol::tree::READ_TEXT => self.text_at(workspace, arguments),
            other => Err(format!(
                "the workspace's folder answers `list` and `read-text`, not `{other}`"
            )),
        }
    }

    /// What `read-text` answers for the file `file` names by its path in the workspace: what it
    /// says, a page of characters at a time (`from`, `next`, `total`).
    fn text_at(
        &self,
        workspace: WorkspaceId,
        arguments: &Map<String, Value>,
    ) -> Result<Value, String> {
        let named = arguments
            .get("file")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|file| !file.is_empty())
            .ok_or("`file` names the file by its path in the workspace")?;
        let from = place(arguments, "from")?.unwrap_or(0);
        let mut found = None;
        tilo_rt::app()
            .block_on(self.workspaces.puts().read(workspace, &mut |tree| {
                found = Some(tree.file_at(named)?);
                Ok(Value::Null)
            }))
            .map_err(|error| error.to_string())??;
        let file = found.ok_or_else(|| format!("the workspace has no {named:?}"))?;
        let bytes = match &file.content {
            Some(content) => self.text_bytes(workspace, content, &file.path)?,
            // A file the tree names no content for is empty, as every device makes one.
            None => Vec::new(),
        };
        let text = String::from_utf8(bytes)
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
        workspace: WorkspaceId,
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
        let bytes = self.text_bytes(workspace, hash, address)?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{address} is not text: {}", read_by(address)))?;
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

    /// The bytes of the content `hash` of `workspace`, which people know as `named`: content put
    /// into the workspace as it was given, or a file of its tree put together from the chunks
    /// its manifest lists, each opened with the workspace's Files keys where they seal it.
    fn text_bytes(
        &self,
        workspace: WorkspaceId,
        hash: &str,
        named: &str,
    ) -> Result<Vec<u8>, String> {
        let missing = || format!("the content of {named:?} has not arrived on this device");
        let stored = self.read(workspace, hash).ok_or_else(missing)?;
        let manifest = if is_manifest(&stored) {
            Some(
                Manifest::decode(&stored)
                    .map_err(|error| format!("the content of {named:?} does not read: {error}"))?,
            )
        } else {
            None
        };
        let size = manifest.as_ref().map_or_else(
            || u64::try_from(stored.len()).unwrap_or(u64::MAX),
            Manifest::total_len,
        );
        if size > MOST_TEXT_BYTES {
            return Err(format!(
                "{named:?} is {size} bytes, and a text read takes at most {MOST_TEXT_BYTES}: {}",
                read_by(named)
            ));
        }
        let keys = match manifest.as_ref().and_then(Manifest::sealed) {
            Some(_) => Some(self.keys(workspace, false)?.ok_or_else(|| {
                "this workspace's files are sealed, and their keys are not here".to_owned()
            })?),
            None => None,
        };
        let files = files_scope(workspace);
        whole_with(
            stored,
            MOST_TEXT_BYTES,
            |chunk| self.shelf.read(&files, &hex::encode(chunk)),
            |sealed, bytes| opened_chunk(workspace, keys.as_ref(), sealed, bytes),
        )
        .ok_or_else(missing)
    }

    /// The bytes `range` of the file `manifest` describes in `workspace`: the chunks it lists
    /// that overlap the range, each opened with the workspace's Files keys where they seal it.
    fn file_range(
        &self,
        workspace: WorkspaceId,
        manifest: &Manifest,
        range: Range<u64>,
        named: &str,
    ) -> Result<Vec<u8>, String> {
        let missing = || format!("the content of {named} has not arrived on this device");
        let keys = match manifest.sealed() {
            Some(_) => Some(self.keys(workspace, false)?.ok_or_else(|| {
                "this workspace's files are sealed, and their keys are not here".to_owned()
            })?),
            None => None,
        };
        let files = files_scope(workspace);
        let mut bytes =
            Vec::with_capacity(usize::try_from(range.end.saturating_sub(range.start)).unwrap_or(0));
        for (_, start, part) in manifest.overlapping(range.clone()) {
            let stored = self
                .shelf
                .read(&files, &hex::encode(part.hash))
                .ok_or_else(missing)?;
            if u64::try_from(stored.len()).unwrap_or(u64::MAX) != manifest.stored_len(part) {
                return Err(format!(
                    "a piece of {named} is not the length its manifest says"
                ));
            }
            let clear = match manifest.sealed() {
                Some(sealed) => opened_chunk(workspace, keys.as_ref(), sealed, &stored)
                    .ok_or_else(|| {
                        format!("a piece of {named} does not open with the workspace's Files keys")
                    })?,
                None => stored,
            };
            let from = usize::try_from(range.start.saturating_sub(start)).unwrap_or(usize::MAX);
            let to = usize::try_from(range.end.saturating_sub(start))
                .unwrap_or(usize::MAX)
                .min(clear.len());
            bytes.extend_from_slice(clear.get(from..to).unwrap_or_default());
        }
        Ok(bytes)
    }
}

/// A chunk of `workspace`'s file sealed as `sealed` says, opened with `keys`; `None` without
/// them, or when it does not open.
fn opened_chunk(
    workspace: WorkspaceId,
    keys: Option<&key_group::ContentKeys>,
    sealed: SealedChunks,
    bytes: &[u8],
) -> Option<Vec<u8>> {
    let metadata = collab_types::EncryptionMetadata::content_group(
        workspace,
        WorkspaceId::from_u128(u128::from_be_bytes(sealed.group)),
        sealed.epoch,
        collab_types::ContentRetention::Delta,
    );
    keys?
        .open_content(
            collab_types::file_content_artifact(workspace),
            &metadata,
            bytes,
        )
        .ok()
        .map(|clear| clear.to_vec())
}

impl OpenedFiles {
    /// These reads as the content gets of the workspaces they read ([`ContentGets`]).
    #[must_use]
    pub fn into_gets(self) -> std::sync::Arc<dyn ContentGets> {
        std::sync::Arc::new(self)
    }
}

impl ContentGets for OpenedFiles {
    /// `length` bytes from `offset` of what `content` names in the caller's workspace: content
    /// put into it as it was given, or a file of its tree, which the tree names by its manifest,
    /// as the file the manifest describes — never the manifest itself.
    fn get(
        &self,
        caller: &Caller,
        content: &str,
        offset: u64,
        length: u64,
    ) -> Result<ContentPiece, ActionCallError> {
        let invalid = |reason: String| ActionCallError::InvalidRequest { reason };
        let named = caller.workspace().ok_or_else(|| {
            invalid("content is read from a workspace, and this caller is in none".into())
        })?;
        let workspace = WorkspaceId::parse(named)
            .ok_or_else(|| invalid(format!("{named:?} names no workspace")))?;
        let hash = tilo_actions::content_hash(content)
            .ok_or_else(|| invalid(format!("{content} is not a tilo.content:<hash> reference")))?;
        // The length asked is the most the caller takes: a piece carries at most what a frame
        // does, and a caller that asked for more reads on from where the piece ends, as it does
        // with any piece shorter than the content.
        let most = MAX_CONTENT_FRAME_BYTES as u64;
        let length = match length {
            0 => most,
            length => length.min(most),
        };
        let stored = self
            .read(workspace, hash)
            .ok_or_else(|| invalid(format!("this workspace has no {content}")))?;
        let manifest = if is_manifest(&stored) {
            Some(
                Manifest::decode(&stored).map_err(|error| ActionCallError::Unavailable {
                    reason: format!("{content} does not read: {error}"),
                })?,
            )
        } else {
            None
        };
        let size = manifest.as_ref().map_or_else(
            || u64::try_from(stored.len()).unwrap_or(u64::MAX),
            Manifest::total_len,
        );
        if offset > size {
            return Err(invalid(format!(
                "the content is {size} bytes: an offset of {offset} is past its end"
            )));
        }
        let end = offset.saturating_add(length).min(size);
        let bytes = if let Some(manifest) = manifest {
            self.file_range(workspace, &manifest, offset..end, content)
                .map_err(|reason| ActionCallError::Unavailable { reason })?
        } else {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let end = usize::try_from(end).unwrap_or(usize::MAX);
            stored.get(start..end).unwrap_or_default().to_vec()
        };
        Ok(ContentPiece {
            size,
            offset,
            bytes,
        })
    }
}
