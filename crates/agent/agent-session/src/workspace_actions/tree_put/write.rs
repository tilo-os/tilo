//! Files' `write-file` on the folder of a workspace whose tree is kept here:
//! `app.so.tilo.file-explorer#write-file`, naming a path in the workspace.
//!
//! On a device `write-file` names the device and writes text to its disk. A caller with no
//! mount (an agent working while every device of its person is off) names a folder in the
//! workspace and no device, and the file is written where the tree is kept: from `text`, or
//! from `content` the workspace keeps already, so what another app answered as content (a
//! PDF, a workbook) becomes a file in a task's folder. It writes over nothing unless `replace`
//! says so, and answers the one call that takes it back, as the verbs by a path do.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{ActionCallError, ActorPlace, Caller, InvokeRequest};
use tilo_content::ContentStore;
use tilo_fs::tree_put::Written;
use tilo_server_protocol::WorkspaceId;

use super::{manifest_of, refused, size_of};
use crate::workspace_actions::{Outcome, Worker, commit_failed, invoke_arguments};

/// The most text one `write-file` takes, as Files on a device does: a longer file is put into
/// the workspace and named by its `content`.
const MOST_TEXT_BYTES: usize = 256 * 1024;

/// The most of a content read whole to see whether it is a file's manifest already.
const MOST_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

/// What the file is written from.
enum Source {
    /// Its text.
    Text(String),
    /// The address of content the workspace keeps.
    Content(String),
}

/// What a `write-file` names.
struct Asked {
    /// The folder it goes into, as a path in the workspace; the workspace's own when empty.
    folder: String,
    /// The name it takes there.
    name: String,
    /// What it is written from.
    source: Source,
    /// Whether a file of that name takes the new content.
    replace: bool,
}

impl Asked {
    /// What `arguments` name of the file to write.
    fn named(arguments: &Map<String, Value>) -> Result<Self, String> {
        let folder = match arguments.get("folder") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(folder)) => folder.trim().to_owned(),
            Some(_) => return Err("`folder` is a path in the workspace, as text".to_owned()),
        };
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("`name` is the file's name")?
            .to_owned();
        let replace = match arguments.get("replace") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(replace)) => *replace,
            Some(_) => return Err("`replace` is true or false".to_owned()),
        };
        let source = match (arguments.get("text"), arguments.get("content")) {
            (Some(Value::String(text)), None | Some(Value::Null)) => {
                if text.len() > MOST_TEXT_BYTES {
                    return Err(format!(
                        "`text` is {} bytes, and `write-file` takes at most {MOST_TEXT_BYTES} as \
                         text: put a longer file into the workspace and name its `content`",
                        text.len()
                    ));
                }
                Source::Text(text.clone())
            }
            (None | Some(Value::Null), Some(Value::String(content))) => {
                let hash = tilo_actions::content_hash(content.trim()).ok_or_else(|| {
                    format!(
                        "`{content}` names no content: give the file's \
                         `tilo.content:<reference>`"
                    )
                })?;
                Source::Content(hash.to_owned())
            }
            _ => {
                return Err(
                    "`write-file` takes the file's `text`, or its `content` as a `tilo.content:` \
                     reference: one of the two"
                        .to_owned(),
                );
            }
        };
        Ok(Self {
            folder,
            name,
            source,
            replace,
        })
    }
}

/// What a file of the tree names the content at `hash` by, and the file's length. Content that
/// is a file's manifest already, with every chunk it lists kept here, is that file: the call
/// that takes a write back names what the file held before so. Any other content is kept as a
/// file of the tree is ([`manifest_of`]).
fn kept(store: &ContentStore, hash: &str) -> Result<(String, u64), String> {
    let size = size_of(store, hash)
        .ok_or_else(|| format!("this workspace holds no tilo.content:{hash}"))?;
    if size <= MOST_MANIFEST_BYTES
        && let Some(bytes) = store.read(hash)
        && fs_content::is_manifest(&bytes)
        && let Ok(manifest) = fs_content::Manifest::decode(&bytes)
        && manifest
            .chunks()
            .iter()
            .all(|chunk| store.has(&hex::encode(chunk.hash)))
    {
        return Ok((hash.to_owned(), manifest.total_len()));
    }
    Ok((manifest_of(store, hash, size)?, size))
}

/// What `write-file` answers: where the file is and how long, whether it took the place of
/// another's content, the folders made on the way, and the one call that takes it back: the
/// trash for a new file (of the outermost folder made for it, when one was), and for a file
/// written over, the write of what it held before.
fn answer(written: &Written, size: u64) -> Value {
    let changed = &written.changed;
    let path = changed
        .entries
        .first()
        .and_then(|entry| entry.to.as_deref());
    let undo = if written.replaced {
        written.was.as_ref().zip(path).map(|(was, path)| {
            let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
            json!({"action": "write-file", "args": {
                "folder": folder,
                "name": name,
                "content": format!("tilo.content:{was}"),
                "replace": true,
            }})
        })
    } else {
        changed
            .made
            .first()
            .map(String::as_str)
            .or(path)
            .map(|taken| json!({"action": "trash", "args": {"folder": "", "entries": [taken]}}))
    };
    let entries: Vec<Value> = changed
        .entries
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "from": entry.from,
                "to": entry.to,
                "folder": entry.folder,
            })
        })
        .collect();
    json!({
        "action": "write-file",
        "path": path,
        "size": size,
        "replaced": written.replaced,
        "entries": entries,
        "made": changed.made,
        "undo": undo,
    })
}

impl Worker {
    /// Answer Files' `write-file` on the folder of the workspace `named`: write the file into
    /// its tree from the text or the content it names, and answer where it now is.
    ///
    /// # Errors
    /// A caller who may not change what Files keeps in the workspace, a call naming a device
    /// (whose own disk only that device writes), neither text nor content or both, content the
    /// workspace does not hold, a path that is none of the workspace, a name that is taken
    /// when `replace` is not asked, a tree this holder cannot read yet, or a change the
    /// workspace did not take.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the holder keeps"
    )]
    pub(in crate::workspace_actions) fn write_tree_file(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        named: &str,
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        if !self.may_in_files(
            workspace_id,
            caller,
            ui::permissions::policy::ActionKind::Undoable,
        ) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let arguments = invoke_arguments(&request.argument);
        if arguments.contains_key("device") {
            return Err(refused(
                "a path in the workspace names no device: a device's own disk is written by \
                 Files on that device",
            ));
        }
        let asked = Asked::named(&arguments).map_err(refused)?;
        let store = self
            .workspace_content(workspace_id)
            .ok_or_else(|| refused("this workspace keeps no files where its tree is kept"))?;
        let (content, size) = match &asked.source {
            Source::Text(text) => {
                let size = u64::try_from(text.len()).unwrap_or(u64::MAX);
                let address = store
                    .put_public(text.as_bytes())
                    .map_err(|error| refused(error.to_string()))?;
                (manifest_of(&store, &address, size).map_err(refused)?, size)
            }
            Source::Content(hash) => kept(&store, hash).map_err(refused)?,
        };
        let (mut tree, held) = self.tree_in_workspace(workspace_id, named)?;
        let now = tilo_clock::unix_now_ns() / 1_000_000;
        let written = tree
            .write_file(
                &asked.folder,
                &asked.name,
                (&content, size),
                asked.replace,
                now,
            )
            .map_err(refused)?;
        let value = answer(&written, size);
        let writes = match &held {
            Some(_) => written.changed.writes,
            // A tree founded now: every document of it from nothing.
            None => tree.document_writes(&BTreeMap::new()),
        };
        self.commit_tree_writes(workspace_id, named, held.as_ref(), writes)
            .map_err(|reason| commit_failed(reason, true))?;
        Ok((
            ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
            None,
        ))
    }
}
