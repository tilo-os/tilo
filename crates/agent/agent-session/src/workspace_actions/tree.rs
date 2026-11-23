//! The files of a workspace's own folder, as the host answers them: its verbs and its reads by
//! a path in the workspace, and `app.so.tilo.file-explorer#open` given a file's `content` and
//! the folder (`dir`) and name it goes under, or many files at once as `files` — a list of
//! those, or a `tilo.content:` reference to one kept in the workspace.
//!
//! A workspace has one folder — its Files app record, the tree its members mount — and those
//! files are its tree. Every change here — a put, a file manager's verb, a file written from
//! text — is made where the tree is, by the host that holds it
//! ([`super::WorkspaceDocumentsLink::change_tree`]), which hands it to the workspace as an
//! ordinary change of the tree; what a read answers is the tree as the workspace has it now
//! ([`super::WorkspaceDocumentsLink::read_tree`]). No app's record is made of a file: what opens
//! it afterwards is the person's choice, from the file itself.

use std::collections::BTreeMap;

use fs_protocol::files::TreeChunk;
use fs_protocol::tree;
use fs_protocol::tree::{FILES_APP, document_id as tree_document};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;
use ui::permissions::policy::{ActionKind, Answer};

use super::{Outcome, Worker, invoke_arguments};

mod by_path;
mod read;
pub(super) mod sealed;
mod write;

/// Whether `target` is one of Files' verbs on the workspace's own folder, by a path in the
/// workspace ([`by_path`]), which change its tree where it is kept; its `write-file` there
/// ([`write`]), which writes a file into it; or one of its reads of that folder ([`read`]),
/// which answer what the tree holds: by a path, on a folder of it, or on a file a
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
        let owner = self.serves_as_owner(caller);
        if owner && !super::bounded::governed(&self.shared, workspace_id) {
            return true;
        }
        let workspace = workspace_id.to_string();
        let who = self.bounded(caller);
        let policy = match &who {
            Some(who) => self.policy_for(workspace_id, who, &workspace, None).ok(),
            None if owner => self
                .shared
                .link
                .get()
                .and_then(|link| link.workspace_policy(workspace_id, caller)),
            None => None,
        };
        let Some(policy) = policy.filter(|policy| policy.workspace == workspace) else {
            return false;
        };
        let document = tree::document_id();
        let node = collab_types::document_node(&workspace, FILES_APP, &document);
        let clock = ui::permissions::policy::Context::at_secs(policy.unix_time);
        if policy.access.answer(&node, ActionKind::Read, &clock) != Answer::Allow
            || (!verbs.is_empty()
                && policy.access.answer(&node, ActionKind::Undoable, &clock) != Answer::Allow)
        {
            return false;
        }
        let permissions = match &who {
            Some(who) => super::bounded::bounded_permissions(&policy, who).ok(),
            None => Some(policy.permissions()),
        };
        permissions.is_some_and(|permissions| {
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

    /// Answer one of Files' verbs on the folder of the workspace, its `write-file`
    /// there, or one of its reads of that folder ([`acts_on_tree`]).
    ///
    /// # Errors
    /// The record of `workspace`'s files: the tree's own (chunk 0), which names the chunks that
    /// hold its nodes.
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

    /// What the verb ([`Self::change_tree`]), the write ([`Self::write_tree_file`]) or the
    /// read ([`Self::read_tree`]) refuses.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the holder keeps"
    )]
    pub(super) fn in_tree(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<
        (
            tilo_actions::ActionOutput,
            Option<tilo_actions::remote::ActorPlace>,
        ),
        Outcome,
    > {
        let action = request.target.address.action.as_str();
        if tree::is_tree_read(action) {
            return self.read_tree(workspace_id, caller, request);
        }
        if action == tree::WRITE_FILE {
            return self.write_tree_file(workspace_id, caller, request);
        }
        self.change_tree(workspace_id, caller, request)
    }

    /// Each file of `puttings` with the manifest the tree names its content by and its length:
    /// what the workspace does not hold of a manifest is kept, a folder of them together. Every
    /// file is named before the tree is touched, so one the workspace holds no content for
    /// refuses the whole open, nothing half put.
    fn named_for_the_tree(
        store: &tilo_content::ContentStore,
        sealing: Option<&sealed::FileSealing>,
        puttings: Vec<Putting>,
    ) -> Result<Vec<(Putting, String, u64)>, String> {
        let mut kept: BTreeMap<String, (u64, String)> = BTreeMap::new();
        let mut pending: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut pending_bytes = 0_usize;
        let mut files = Vec::with_capacity(puttings.len());
        for putting in puttings {
            let (size, manifest) = if let Some(named) = kept.get(&putting.hash).cloned() {
                named
            } else {
                let size = size_of(store, &putting.hash)
                    .ok_or_else(|| format!("this workspace holds no {}", putting.content))?;
                // A file of the tree is named by its content manifest, as a mounted computer
                // names one, so every member reads it the same way; files of the same content
                // are named once, and a folder's manifests are kept together.
                let (manifest, added) =
                    manifest_into(store, &putting.hash, size, &mut pending, sealing)?;
                pending_bytes = pending_bytes.saturating_add(added);
                let named = (size, manifest);
                kept.insert(putting.hash.clone(), named.clone());
                named
            };
            files.push((putting, manifest, size));
            if pending_bytes >= MANIFEST_FLUSH_BYTES {
                keep_pending(store, &mut pending)?;
                pending_bytes = 0;
            }
        }
        keep_pending(store, &mut pending)?;
        Ok(files)
    }

    /// Answer a Files open naming a file: put it into the workspace's tree at the folder and
    /// under the name it asks for, and answer where it now is.
    ///
    /// The tree's one writer is the host that holds it ([`WorkspaceDocumentsLink::change_tree`]);
    /// this answers the content and where it goes, never a second way into the tree.
    ///
    /// # Errors
    /// A caller who may not change the workspace's records, content the workspace does not hold,
    /// or a put the tree refuses.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn put_in_workspace(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
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
        let arguments = invoke_arguments(&request.argument);
        let many = arguments.contains_key("files");
        let store = self
            .workspace_content(workspace_id)
            .ok_or_else(|| refused("this workspace keeps no files on its host"))?;
        let puttings = Putting::all(&arguments, &store)?;
        // The tree of a governed workspace is sealed: its keys are what reading it and writing
        // to it need.
        let sealing = self.file_sealing(workspace_id, true).map_err(refused)?;
        let files =
            Worker::named_for_the_tree(&store, sealing.as_ref(), puttings).map_err(refused)?;
        self.check_file_keys(workspace_id, sealing.as_ref())
            .map_err(refused)?;
        if !self.may_on_tree(
            workspace_id,
            caller,
            &[verbs::NEW_FOLDER, verbs::NEW_FILE, verbs::WRITE],
        ) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let mtime = tilo_clock::unix_now_ns() / 1_000_000;
        // Every file goes in as one change of the tree: one write per record
        // it touched, however many files went into it.
        let to_put: Vec<tilo_fs::tree_sync::FileToPut<'_>> = files
            .iter()
            .map(|(putting, manifest, size)| tilo_fs::tree_sync::FileToPut {
                to: &putting.dir,
                name: &putting.name,
                hash: manifest,
                size: *size,
                mtime,
            })
            .collect();
        let link = self
            .shared
            .link
            .get()
            .cloned()
            .ok_or_else(|| refused("this host keeps no workspace's tree"))?;
        let value = link
            .change_tree(
                workspace_id,
                Some(acting_context(caller, request)),
                &mut |tree| {
                    let placed = tree.put_files(&to_put)?;
                    // A folder's answer names how many files it put, not each: the caller sent
                    // the list and knows where each file is, and the answer to a folder of
                    // twenty thousand stays one bounded message.
                    Ok(if many {
                        json!({"count": placed.len()})
                    } else {
                        files.iter().zip(placed.iter()).next().map_or_else(
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
                    })
                },
            )
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
    mut reader: impl std::io::Read,
    size: u64,
    sealing: Option<&sealed::FileSealing>,
) -> Result<Vec<u8>, String> {
    if let Some(sealing) = sealing {
        return sealing.manifest(store, reader, size);
    }
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
    sealing: Option<&sealed::FileSealing>,
) -> Result<String, String> {
    let reader = store
        .open(hash)
        .ok_or_else(|| format!("this workspace holds no tilo.content:{hash}"))?;
    let encoded = manifest_bytes(store, reader, size, sealing)?;
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
    sealing: Option<&sealed::FileSealing>,
) -> Result<(String, usize), String> {
    let reader = store
        .open(hash)
        .ok_or_else(|| format!("this workspace holds no tilo.content:{hash}"))?;
    let encoded = manifest_bytes(store, reader, size, sealing)?;
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
