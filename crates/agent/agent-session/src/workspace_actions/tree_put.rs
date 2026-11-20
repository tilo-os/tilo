//! Putting a file into a workspace's file tree: `app.so.tilo.file-explorer#open`
//! given a file's `content`, and the folder (`dir`) and name it goes under, or
//! many files at once as `files` — a list of those, or a `tilo.content:`
//! reference to one kept in the workspace — in one change of the tree.
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
use ui::permissions::policy::{ActionKind, Answer};

use super::{Outcome, Worker, commit_failed, invoke_arguments};

mod by_path;
mod read;
mod write;

/// Whether `target` is one of Files' verbs on the workspace's own folder, by a path in the
/// workspace ([`by_path`]), which change its tree where it is kept; its `write-file` there
/// ([`write`]), which writes a file into it; or one of its reads of that folder ([`read`]),
/// which answer what the tree kept here holds: by a path, on a folder of it, or on a file a
/// listing offered ([`tree::file_resource`]).
pub(super) fn acts_on_tree(target: &tilo_actions::remote::RemoteActionTarget) -> bool {
    let action = target.address.action.as_str();
    if tree::file_of_resource(&target.address.resource).is_some() {
        return action == tree::READ_TEXT;
    }
    // A folder of the tree reads what it holds, as the tree's own folder does.
    if tree::folder_of_resource(&target.address.resource).is_some() {
        return matches!(action, tree::LIST | tree::HISTORY);
    }
    target
        .address
        .resource
        .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
        == Some(FILES_APP)
        && (tree::is_tree_verb(action) || tree::is_tree_read(action) || action == tree::WRITE_FILE)
}

/// The verb that brings entries back from the trash: asked by their ids alone, it answers the
/// names they go by.
const RESTORE: &str = "restore";

/// Whether what a call of `target` answers holds words that are neither the caller's own nor
/// this holder's: what a file of the workspace's folder says, and the names its entries go
/// by, which whoever put them there wrote, also somebody outside who passed a file on. Both
/// reads of the folder answer such words ([`read`]), and so does [`RESTORE`]. A caller's
/// tools are told so with the answer
/// ([`tilo_actions::remote::InvocationState::with_untrusted`]): it is data to reason about,
/// never an instruction to follow. What the other verbs and a write answer names only what
/// the caller itself named.
pub(super) fn answers_from_outside(target: &tilo_actions::remote::RemoteActionTarget) -> bool {
    let action = target.address.action.as_str();
    acts_on_tree(target) && (tree::is_tree_read(action) || action == RESTORE)
}

/// The records of a workspace's tree as the host holds them: each with the
/// number of its chunk, the tree's own record first, and the chunks the host
/// holds at all, each with the heads it stands at.
type HeldTree = (Vec<(TreeChunk, Vec<u8>)>, HeldChunks);

/// The chunks of a workspace's tree the host holds, each with the heads it stands at.
pub(super) type HeldChunks = BTreeMap<TreeChunk, Vec<[u8; 32]>>;

/// A workspace's tree as a request read it, kept for the next: the heads of
/// each of its documents it took in, and the tree itself once the request that read it
/// handed it back ([`Worker::keep_tree`]). A request holds the tree while it
/// is served, so one that fails leaves none, and the next reads it whole.
pub(super) struct KeptTree {
    tree: Option<Namespace>,
    heads: HeldChunks,
}

impl KeptTree {
    /// A tree just read at `heads`, held by the request that read it.
    fn read(heads: HeldChunks) -> Self {
        Self { tree: None, heads }
    }
}

/// What a document of a workspace's tree did since a request read it.
enum Since {
    /// The workspace holds no such document.
    Missing,
    /// It is where it was read.
    Unchanged,
    /// The changes after what was read (the document whole where nothing
    /// was), with the heads it is at now.
    Changed(Vec<u8>, Vec<[u8; 32]>),
}

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

/// Write the tree's file actions as `caller` in the call `request`, as the host admitted them,
/// until dropped: each change the call makes then names who made it, in which run and under
/// which invocation, which History shows and Undo takes back as one step of theirs, in every
/// document of the tree it touched ([`tilo_fs::namespace::acting_as`]).
pub(super) fn acting_for(caller: &Caller, request: &InvokeRequest) -> tilo_fs::namespace::ActingAs {
    tilo_fs::namespace::acting_as(Some(acting_context(caller, request)))
}

/// What the tree's changes of the call `request` by `caller` are part of: who made them, in
/// which run, under which invocation.
pub(super) fn acting_context(
    caller: &Caller,
    request: &InvokeRequest,
) -> tilo_actions::ActionContext {
    let invoker = tilo_actions::ActionInvoker::admitted(caller.acting())
        .named(caller.agent_name().map(str::to_owned));
    super::caller_context(caller, request)
        .with_invoker(invoker)
        .with_source_invocation(Some(request.invocation))
}

fn refused(reason: impl Into<String>) -> Outcome {
    Outcome::Rejected(ActionCallError::Refused {
        reason: reason.into(),
    })
}

/// The most files one open puts into the tree: a bounded request and change,
/// answered within the bound a workspace's actions answer in. A larger folder
/// goes in several opens.
const MAX_FILES: usize = 10_000;

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
    /// What `arguments` name of the files to put into the workspace: the one
    /// file they name, or each of `files`, a list or a `tilo.content:`
    /// reference to one kept in the workspace.
    ///
    /// # Errors
    /// As [`Self::named`] for any of them, a list that does not read or the
    /// workspace does not hold, or one of more files than a bounded request
    /// and change carries.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn all(
        arguments: &Map<String, Value>,
        store: &tilo_content::ContentStore,
    ) -> Result<Vec<Self>, Outcome> {
        let Some(files) = arguments.get("files") else {
            return Ok(vec![Self::named(arguments)?]);
        };
        let listed: Vec<Value> = match files {
            Value::Array(list) => list.clone(),
            // A folder's list is data like the files it names: it goes up as
            // content, so the change names one reference however many files it
            // puts into the tree.
            Value::String(content) => {
                let content = content.trim();
                let hash = tilo_actions::content_hash(content).ok_or_else(|| {
                    refused(format!(
                        "`{content}` names no list of files: give a JSON list of content, name and dir"
                    ))
                })?;
                let bytes = store
                    .read(hash)
                    .ok_or_else(|| refused(format!("this workspace holds no {content}")))?;
                serde_json::from_slice(&bytes)
                    .map_err(|error| refused(format!("{content} is no list of files: {error}")))?
            }
            _ => {
                return Err(refused(
                    "`files` is a list of files, or a tilo.content: reference to one",
                ));
            }
        };
        if listed.is_empty() || listed.len() > MAX_FILES {
            return Err(refused(format!(
                "one open puts 1 to {MAX_FILES} files, not {}",
                listed.len()
            )));
        }
        listed
            .iter()
            .map(|file| {
                file.as_object()
                    .ok_or_else(|| refused("each of `files` is a file: its content, name and dir"))
                    .and_then(Self::named)
            })
            .collect()
    }

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
    /// Whether `caller` may do each of `verbs` (the tree's file actions,
    /// [`fs_protocol::file_action::verbs`]) to the files of the workspace `workspace_id`, or
    /// with none named read them: decided at each action's own node of the workspace's tree
    /// as every other action is, the person this holder works for as the owner, anyone else
    /// as the workspace's policy answers now, an agent held to its bound.
    pub(super) fn may_on_tree(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        verbs: &[&str],
    ) -> bool {
        if self.serves_as_owner(caller) {
            return true;
        }
        let Some(who) = self.bounded(caller) else {
            return false;
        };
        self.permissions_across(workspace_id, &who, &workspace_id.to_string())
            .is_some_and(|permissions| {
                // The nodes a mount writing a file and every receiver of the change decide it at.
                if verbs.is_empty() {
                    return permissions.answer(
                        &fs_protocol::file_action::verb_node(None, tree::LIST),
                        ActionKind::Read,
                    ) == Answer::Allow;
                }
                verbs.iter().all(|verb| {
                    permissions.answer(
                        &fs_protocol::file_action::verb_node(None, verb),
                        ActionKind::Undoable,
                    ) == Answer::Allow
                })
            })
    }

    /// Answer one of Files' verbs on the folder of the workspace `named`, its `write-file`
    /// there, or one of its reads of that folder ([`acts_on_tree`]).
    ///
    /// # Errors
    /// What the verb ([`Self::change_tree`]), the write ([`Self::write_tree_file`]) or the
    /// read ([`Self::read_tree`]) refuses.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the holder keeps"
    )]
    pub(super) fn in_tree(
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
        let action = request.target.address.action.as_str();
        if tree::is_tree_read(action) {
            return self.read_tree(workspace_id, caller, request, named);
        }
        if action == tree::WRITE_FILE {
            return self.write_tree_file(workspace_id, caller, request, named);
        }
        self.change_tree(workspace_id, caller, request, named)
    }

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
    ///
    /// The tree a request on this workspace kept ([`Self::keep_tree`]) takes
    /// in only what changed of each of its documents since it was read, so a request
    /// costs what changed, not the whole tree; a tree nobody kept is read
    /// whole.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn tree_with_heads(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
    ) -> Result<(Namespace, Option<HeldChunks>), Outcome> {
        let key = (workspace_id, workspace.to_owned());
        if let Some(kept) = self.trees.get_mut(&key)
            && let Some(tree) = kept.tree.take()
        {
            let heads = kept.heads.clone();
            if let Some((tree, held)) = self.caught_up(workspace_id, workspace, tree, heads)? {
                if let Some(kept) = self.trees.get_mut(&key) {
                    kept.heads.clone_from(&held);
                }
                return Ok((tree, Some(held)));
            }
        }
        let author = hex::encode(self.own_device.key.verifying_key().to_bytes());
        // Every document the tree is kept as names itself as Files' document of this workspace,
        // as a page's Files window names the one it founds: a tree a call wrote is then one every
        // page opens, not one whose identity it refuses.
        let mount = tilo_fs::namespace::MountAuthor::device(author).in_workspace(workspace);
        let Some((records, held)) = self.tree_records(workspace_id, workspace)? else {
            let tree = Namespace::new(mount)
                .map_err(|error| refused(format!("a tree could not be founded: {error}")))?;
            self.trees.insert(key, KeptTree::read(BTreeMap::new()));
            return Ok((tree, None));
        };
        let tree = Namespace::from_records(mount, records)
            .map_err(|error| refused(format!("the workspace's tree does not read: {error}")))?;
        self.trees.insert(key, KeptTree::read(held.clone()));
        Ok((tree, Some(held)))
    }

    /// Keep `tree`, which a request read with [`Self::tree_with_heads`] and
    /// whose writes the workspace took, for the next request on it. A request
    /// whose writes failed keeps nothing: the next reads the tree whole.
    pub(super) fn keep_tree(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
        tree: Namespace,
    ) {
        if let Some(kept) = self.trees.get_mut(&(workspace_id, workspace.to_owned())) {
            kept.tree = Some(tree);
        }
    }

    /// `tree`, read when its documents were at `heads`, with what changed of
    /// them since taken in, and the heads they are at now; `None` when the
    /// workspace holds none of the tree's own document, which a whole read
    /// settles.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome is what the request is answered with"
    )]
    fn caught_up(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
        mut tree: Namespace,
        mut heads: HeldChunks,
    ) -> Result<Option<(Namespace, HeldChunks)>, Outcome> {
        let unreadable = |error: automerge::AutomergeError| {
            refused(format!("the workspace's tree does not read: {error}"))
        };
        let index = Self::tree_route(workspace);
        match self.since(workspace_id, &index, true, heads.get(&0))? {
            Since::Missing => return Ok(None),
            Since::Unchanged => {}
            Since::Changed(bytes, at) => {
                tree.take_documents(vec![(0, bytes)]).map_err(unreadable)?;
                heads.insert(0, at);
            }
        }
        let mut taken = Vec::new();
        for chunk in tree.listed_chunks() {
            let route = Self::tree_chunk_route(workspace, chunk);
            if let Since::Changed(bytes, at) =
                self.since(workspace_id, &route, false, heads.get(&chunk))?
            {
                taken.push((chunk, bytes));
                heads.insert(chunk, at);
            }
        }
        if !taken.is_empty() {
            tree.take_documents(taken).map_err(unreadable)?;
        }
        Ok(Some((tree, heads)))
    }

    /// What changed of the document at `route` since the heads `since`.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome is what the request is answered with"
    )]
    fn since(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        act: bool,
        since: Option<&Vec<[u8; 32]>>,
    ) -> Result<Since, Outcome> {
        let held = self.refreshed_for(workspace_id, route, act)?;
        match held.update(route, since.map(Vec::as_slice)) {
            Ok(Some((
                DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes),
                at,
            ))) => Ok(Since::Changed(bytes, at)),
            Ok(None) if since.is_some() => Ok(Since::Unchanged),
            Ok(None) | Err(_) if !self.held_or_owed(workspace_id, route)? => Ok(Since::Missing),
            // Held, and not readable yet (its keys, or the rest of its history, on their way):
            // read as unchanged with nothing taken, a put founded it again over the
            // workspace's own, which the workspace refused as one it holds already.
            Ok(None) => Err(Outcome::Rejected(ActionCallError::NotReady)),
            Err(why) => Err(Outcome::Rejected(ActionCallError::Unavailable {
                reason: why,
            })),
        }
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
        let mut doc = AutoCommit::load(&index)
            .map_err(|error| refused(format!("the workspace's tree does not read: {error}")))?;
        if doc.get_heads().is_empty() {
            // A document a writer founded and holds no change of yet: the tree nobody wrote,
            // which the write founds with the identity every founder derives.
            return Ok(None);
        }
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
            // What the tree's document stands at before this write: the identity goes onto
            // it, so the change it makes follows the document's own.
            let prior = match self.refreshed_for(workspace_id, &route, true) {
                Ok(state) => state.document(&route).ok(),
                Err(_) => None,
            };
            if prior.is_none() && held.is_some_and(|held| held.contains(&write.chunk)) {
                return Err("the workspace's tree could not be read to name it".into());
            }
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
        use fs_protocol::file_action::verbs;
        use tilo_actions::ActionOutput;
        if !self.may_on_tree(
            workspace_id,
            caller,
            &[verbs::NEW_FOLDER, verbs::NEW_FILE, verbs::WRITE],
        ) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let _acting = acting_for(caller, request);
        let arguments = invoke_arguments(&request.argument);
        let many = arguments.contains_key("files");
        let store = self
            .workspace_content(workspace_id)
            .ok_or_else(|| refused("this workspace keeps no files on its host"))?;
        let puttings = Putting::all(&arguments, &store)?;
        // Every file is named before the tree is touched: one the workspace
        // holds no content for refuses the whole open, nothing half put. What
        // the store does not hold yet — each file's manifest — is kept
        // together, one write for all.
        let mut kept: std::collections::BTreeMap<String, (u64, String)> =
            std::collections::BTreeMap::new();
        let mut pending: std::collections::BTreeMap<String, Vec<u8>> =
            std::collections::BTreeMap::new();
        let mut pending_bytes = 0_usize;
        let mut files = Vec::with_capacity(puttings.len());
        for putting in puttings {
            let known = kept.get(&putting.hash).cloned();
            let (size, manifest) = if let Some(named) = known {
                named
            } else {
                let size = size_of(&store, &putting.hash).ok_or_else(|| {
                    refused(format!("this workspace holds no {}", putting.content))
                })?;
                // A file of the tree is named by its content manifest, as a
                // mounted computer names one, so every member reads it the
                // same way; files of the same content are named once, and a
                // folder's manifests are kept together.
                let (manifest, added) =
                    manifest_into(&store, &putting.hash, size, &mut pending).map_err(refused)?;
                pending_bytes = pending_bytes.saturating_add(added);
                let named = (size, manifest);
                kept.insert(putting.hash.clone(), named.clone());
                named
            };
            files.push((putting, manifest, size));
            if pending_bytes >= MANIFEST_FLUSH_BYTES {
                keep_pending(&store, &mut pending).map_err(refused)?;
                pending_bytes = 0;
            }
        }
        keep_pending(&store, &mut pending).map_err(refused)?;
        // The tree of a governed workspace is sealed: its keys are what
        // reading it and writing to it need.
        let (mut tree, held) = self.tree_in_workspace(workspace_id, named)?;
        let mtime = tilo_clock::unix_now_ns() / 1_000_000;
        // Every file goes in before anything is written: one write per record
        // of the tree that changed, however many files went into it.
        let to_put: Vec<tilo_fs::tree_put::FileToPut<'_>> = files
            .iter()
            .map(|(putting, manifest, size)| tilo_fs::tree_put::FileToPut {
                to: &putting.dir,
                name: &putting.name,
                hash: manifest,
                size: *size,
                mtime,
            })
            .collect();
        let put = tree.put_files(&to_put).map_err(refused)?;
        let writes = match &held {
            Some(_) => put.writes,
            // A tree founded now: every record of it from nothing, the root id
            // the tree's own record carries included.
            None => tree.document_writes(&std::collections::BTreeMap::new()),
        };
        self.commit_tree_writes(workspace_id, named, held.as_ref(), writes)
            .map_err(|reason| commit_failed(reason, true))?;
        self.keep_tree(workspace_id, named, tree);
        // A folder's answer names how many files it put, not each: the caller
        // sent the list and knows where each file is, and the answer to a
        // folder of twenty thousand stays one bounded message.
        let value = if many {
            json!({"count": put.files.len()})
        } else {
            files.iter().zip(put.files.iter()).next().map_or_else(
                Value::default,
                |((putting, _, size), file)| {
                    json!({
                        "path": file.path,
                        "content": putting.content,
                        "size": size,
                        "replaced": file.replaced,
                    })
                },
            )
        };
        Ok((
            ActionOutput {
                value: Some(value),
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

/// The manifest of the content `hash` names, `size` bytes long, as a file of
/// the tree is named: chunked, each chunk the store holds none of kept as its
/// end is known — a file of a gigabyte costs what one chunk does in memory,
/// not the file — and the manifest listing them answered encoded.
///
/// # Errors
/// The content is not in `store`, is not `size` bytes long, or what was kept
/// is not at the address the manifest names.
fn manifest_bytes(
    store: &tilo_content::ContentStore,
    hash: &str,
    size: u64,
) -> Result<Vec<u8>, String> {
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
    Ok(manifest.encode())
}

/// Keep the content `hash` names as a file of the tree is kept: its manifest
/// kept in the store, the manifest's address answered.
///
/// # Errors
/// As [`manifest_bytes`].
fn manifest_of(
    store: &tilo_content::ContentStore,
    hash: &str,
    size: u64,
) -> Result<String, String> {
    let encoded = manifest_bytes(store, hash, size)?;
    store
        .put_public(&encoded)
        .map_err(|error| error.to_string())
}

/// How much content one [`keep_pending`] keeps at once: a folder's manifests,
/// whose bytes are small.
const MANIFEST_FLUSH_BYTES: usize = 8 * 1024 * 1024;

/// Name the content `hash` names as a file of the tree is named: its manifest
/// collected in `pending` under its own BLAKE3 address, that address answered
/// with how many bytes were added. A mounted computer, a page and the
/// workspace's host read a file of the tree from its manifest's chunks.
///
/// # Errors
/// The content is not in `store`, or it does not read as the file it is named.
fn manifest_into(
    store: &tilo_content::ContentStore,
    hash: &str,
    size: u64,
    pending: &mut std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<(String, usize), String> {
    let encoded = manifest_bytes(store, hash, size)?;
    let address = hex::encode(blake3::hash(&encoded).as_bytes());
    if store.has(&address) || pending.contains_key(&address) {
        return Ok((address, 0));
    }
    let added = encoded.len();
    pending.insert(address.clone(), encoded);
    Ok((address, added))
}

/// Keep what [`manifest_into`] collected, one store write for all of it: a
/// folder of files waits on the disk once rather than once per file.
fn keep_pending(
    store: &tilo_content::ContentStore,
    pending: &mut std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    if pending.is_empty() {
        return Ok(());
    }
    let addresses: Vec<String> = pending.keys().cloned().collect();
    let items: Vec<&[u8]> = pending.values().map(Vec::as_slice).collect();
    let kept = store.put_public_many(&items);
    if kept.len() != addresses.len() {
        return Err("the store did not answer for every kept file".into());
    }
    for (address, kept) in addresses.iter().zip(kept) {
        let kept = kept.map_err(|error| error.to_string())?;
        if kept != *address {
            return Err(format!("content was kept at {kept}, not {address}"));
        }
    }
    pending.clear();
    Ok(())
}

/// How much of a file [`manifest_bytes`] reads at a time.
const PIECE: usize = 1024 * 1024;
