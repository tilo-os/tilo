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

use std::collections::{BTreeMap, BTreeSet};

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use fs_protocol::files::TreeChunk;
use fs_protocol::tree;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_fs::namespace::Namespace;
use tilo_fs::tree_document::{FILES_APP, document_id as tree_document};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

use super::{Outcome, Worker, commit_failed, invoke_arguments};

mod by_path;

/// Whether `target` is one of Files' verbs on the workspace's own folder, by a path in the
/// workspace ([`by_path`]): they change its tree where it is kept.
pub(super) fn acts_on_tree(target: &tilo_actions::remote::RemoteActionTarget) -> bool {
    target
        .address
        .resource
        .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
        == Some(FILES_APP)
        && tree::is_tree_verb(&target.address.action)
}

/// The records of a workspace's tree as the host holds them: each with the
/// number of its chunk, the tree's own record first, and the chunks the host
/// holds at all, each with the heads it stands at.
type HeldTree = (Vec<(TreeChunk, Vec<u8>)>, HeldChunks);

/// The chunks of a workspace's tree the host holds, each with the heads it stands at.
pub(super) type HeldChunks = BTreeMap<TreeChunk, Vec<[u8; 32]>>;

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
    Outcome::Rejected(ActionCallError::Refused {
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
    /// The record of `workspace`'s files: the tree's own (chunk 0), which names
    /// the chunks that hold its nodes.
    pub(super) fn tree_route(workspace: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: FILES_APP.to_owned(),
            cluster_id: workspace.to_owned(),
            document: tree_document(),
        }
    }

    /// The record of chunk `chunk` of `workspace`'s files.
    pub(super) fn tree_chunk_route(workspace: &str, chunk: TreeChunk) -> DocumentRoute {
        DocumentRoute {
            document: tree::chunk_document_id(chunk),
            ..Self::tree_route(workspace)
        }
    }

    /// The tree of `workspace`, as the records this host holds of it read, with
    /// which records those are; `None` for a tree nobody wrote yet, which the
    /// put founds.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn tree_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
    ) -> Result<(Namespace, Option<BTreeSet<TreeChunk>>), Outcome> {
        let (tree, held) = self.tree_with_heads(workspace_id, workspace)?;
        Ok((tree, held.map(|held| held.into_keys().collect())))
    }

    /// [`Self::tree_in_workspace`], with the heads each held record stands at.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn tree_with_heads(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
    ) -> Result<(Namespace, Option<HeldChunks>), Outcome> {
        let author = hex::encode(self.host_device.key.verifying_key().to_bytes());
        let Some((records, held)) = self.tree_records(workspace_id, workspace)? else {
            let tree = Namespace::new(&author)
                .map_err(|error| refused(format!("a tree could not be founded: {error}")))?;
            return Ok((tree, None));
        };
        let tree = Namespace::from_records(&author, records)
            .map_err(|error| refused(format!("the workspace's tree does not read: {error}")))?;
        Ok((tree, Some(held)))
    }

    /// The records of `workspace`'s tree that this host holds, the tree's own
    /// first, with which of them it holds; `None` for a tree nobody wrote yet. A
    /// chunk the tree's own record lists that the host holds none of is read as
    /// an empty one: the write founds it, and what other members write there
    /// merges with it.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn tree_records(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
    ) -> Result<Option<HeldTree>, Outcome> {
        let Some((index, heads)) =
            self.saved_here(workspace_id, &Self::tree_route(workspace), true)?
        else {
            return Ok(None);
        };
        let doc = AutoCommit::load(&index)
            .map_err(|error| refused(format!("the workspace's tree does not read: {error}")))?;
        let mut records = vec![(0, index)];
        let mut held = BTreeMap::from([(0, heads)]);
        for chunk in tree::listed_chunks(&doc) {
            match self.saved_here(
                workspace_id,
                &Self::tree_chunk_route(workspace, chunk),
                false,
            )? {
                Some((bytes, heads)) => {
                    held.insert(chunk, heads);
                    records.push((chunk, bytes));
                }
                None => records.push((chunk, AutoCommit::new().save())),
            }
        }
        Ok(Some((records, held)))
    }

    /// Commit `writes` to `workspace`'s tree, one record each: a record not in `held` (none
    /// for a tree nobody wrote yet) is founded first with the empty genesis every writer makes
    /// alike, then takes what was written there, as a record founded on the session is.
    pub(super) fn commit_tree_writes(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
        held: Option<&BTreeSet<TreeChunk>>,
        writes: Vec<tilo_fs::namespace::ResourceWrite>,
    ) -> Result<(), String> {
        for write in writes {
            if write.changes.is_empty() {
                continue;
            }
            let route = Self::tree_chunk_route(workspace, write.chunk);
            if !held.is_some_and(|held| held.contains(&write.chunk)) {
                let genesis = DocumentMutation::Snapshot(AutoCommit::new().save());
                self.commit(workspace_id, &route, &genesis)?;
            }
            self.commit(
                workspace_id,
                &route,
                &DocumentMutation::Incremental(write.changes),
            )?;
        }
        Ok(())
    }

    /// Answer a Files open naming a file: put it into `named`'s tree at the
    /// folder and under the name it asks for, founding the tree's records when
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
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let putting = Putting::named(&invoke_arguments(&request.argument))?;
        let store = self
            .workspace_content(workspace_id)
            .ok_or_else(|| refused("this workspace keeps no files on its host"))?;
        let size = size_of(&store, &putting.hash)
            .ok_or_else(|| refused(format!("this workspace holds no {}", putting.content)))?;
        // A file of the tree is named by its content manifest, as a mounted
        // computer names one, so every member reads it the same way.
        let manifest = manifest_of(&store, &putting.hash, size).map_err(refused)?;
        // The tree of a governed workspace is sealed: its keys are what
        // reading it and writing to it need.
        let (mut tree, held) = self.tree_in_workspace(workspace_id, named)?;
        let mtime = tilo_clock::unix_now_ns() / 1_000_000;
        let put = tree
            .put_file(&putting.dir, &putting.name, &manifest, size, mtime)
            .map_err(refused)?;
        let writes = match &held {
            Some(_) => put.writes,
            // A tree founded now: every record of it from nothing, the root id
            // the tree's own record carries included.
            None => tree.document_writes(&std::collections::BTreeMap::new()),
        };
        self.commit_tree_writes(workspace_id, named, held.as_ref(), writes)
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

/// The length of the content `hash` names: what the store knows of it, or,
/// for sealed content, whose length only its seal keeps, counted as it is
/// read through, a piece at a time.
fn size_of(store: &tilo_content::ContentStore, hash: &str) -> Option<u64> {
    if let Some(size) = store.size(hash) {
        return Some(size);
    }
    let mut reader = store.open(hash)?;
    std::io::copy(&mut reader, &mut std::io::sink()).ok()
}

/// Keep the content `hash` names, `size` bytes long, as a file of the tree is
/// kept: chunked, each chunk and the manifest listing them under their own
/// BLAKE3 address in `store`, the manifest's address answered. A mounted
/// computer, a page and the workspace's host read a file of the tree from its
/// manifest's chunks.
///
/// The bytes are read and chunked a piece at a time, each chunk kept as soon
/// as its end is known: a file of a gigabyte costs what one chunk does in
/// memory, not the file.
///
/// # Errors
/// The content is not in `store`, is not `size` bytes long, or what was kept
/// is not at the address the manifest names.
fn manifest_of(
    store: &tilo_content::ContentStore,
    hash: &str,
    size: u64,
) -> Result<String, String> {
    use std::io::Read as _;
    let mut reader = store
        .open(hash)
        .ok_or_else(|| format!("this workspace holds no tilo.content:{hash}"))?;
    let mut failed: Option<String> = None;
    let mut keep = |chunk: &fs_content::ChunkRef, bytes: &[u8]| {
        if failed.is_some() {
            return;
        }
        let address = hex::encode(chunk.hash);
        if store.has(&address) {
            return;
        }
        match store.put_public(bytes) {
            Ok(kept) if kept == address => {}
            Ok(kept) => failed = Some(format!("a chunk was kept at {kept}, not at {address}")),
            Err(error) => failed = Some(error.to_string()),
        }
    };
    let mut stream = fs_content::Streamed::new(size, None);
    let mut piece = vec![0_u8; PIECE];
    loop {
        let read = match reader.read(&mut piece) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        };
        stream
            .push(piece.get(..read).unwrap_or_default(), &mut keep)
            .map_err(|error| error.to_string())?;
    }
    let (manifest, _) = stream
        .finish(&mut keep)
        .map_err(|error| error.to_string())?;
    if let Some(failed) = failed {
        return Err(failed);
    }
    store
        .put_public(&manifest.encode())
        .map_err(|error| error.to_string())
}

/// How much of a file [`manifest_of`] reads at a time.
const PIECE: usize = 1024 * 1024;
