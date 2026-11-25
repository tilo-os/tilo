//! Files a `files#open` names, put into the workspace's own folder; and Files' reads and verbs of
//! that folder by paths in the workspace (`reads`, `changes`).
//!
//! A workspace has one folder — its Files app record, the tree its members mount — and a put
//! writes a node into it: a folder `dir` that is not there yet is made on the way, a file of the
//! same name takes the new content, and no app's record is made of the file. Every file is named
//! before the tree is touched, so content the workspace does not hold refuses the whole open,
//! nothing half put.
//!
//! A file of the tree is named by its content manifest, as a mounted computer names one, so
//! every member reads it the same way; the manifest's chunks are kept on the shelf the
//! workspace's file content lives on, sealed with the workspace's Files keys where the workspace
//! seals its records. Files of the same content are named once.

use std::collections::BTreeMap;
use std::sync::Arc;

use fs_protocol::tree::{self, FILES_APP};
use serde_json::{Map, Value, json};
use tilo_actions::ActionContext;
use tilo_broker_core::services::CONTENT_PUT_SCOPE;
use tilo_fs::content::{ChunkRef, Manifest, SealedChunks, Streamed};
use tilo_fs::namespace::FileToPut;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

use crate::content::{ContentScope, ContentShelf};
use crate::workspaces::Workspaces;
use tilo_broker_core::services::{FilesEntry, FilesPutCall};

/// The most files one open puts into the tree: a bounded request and change,
/// answered within the bound a workspace's actions answer in. A larger folder
/// goes in several opens.
const MAX_FILES: usize = 10_000;

/// How much of a file is read at a time: a file of a gigabyte costs what one
/// piece does in memory, not the file.
const PIECE: u64 = 1024 * 1024;

/// What a `files` open names of the file it puts into the workspace.
struct Putting {
    /// Its `tilo.content:<hash>` reference.
    content: String,
    /// The hash the reference names.
    hash: String,
    /// The folder in the tree it goes into, `""` for the workspace's root.
    dir: String,
    /// The name it takes in that folder.
    name: String,
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
    fn all(
        arguments: &Map<String, Value>,
        store: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<Vec<Self>, String> {
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
                    format!(
                        "`{content}` names no list of files: give a JSON list of content, name and dir"
                    )
                })?;
                let bytes =
                    store(hash).ok_or_else(|| format!("this workspace holds no {content}"))?;
                serde_json::from_slice(&bytes)
                    .map_err(|error| format!("{content} is no list of files: {error}"))?
            }
            _ => {
                return Err(
                    "`files` is a list of files, or a tilo.content: reference to one".to_owned(),
                );
            }
        };
        if listed.is_empty() || listed.len() > MAX_FILES {
            return Err(format!(
                "one open puts 1 to {MAX_FILES} files, not {}",
                listed.len()
            ));
        }
        listed
            .iter()
            .map(|file| {
                file.as_object()
                    .ok_or_else(|| {
                        "each of `files` is a file: its content, name and dir".to_owned()
                    })
                    .and_then(Self::named)
            })
            .collect()
    }

    /// What `arguments` name of the file to put into the workspace.
    ///
    /// # Errors
    /// No content reference, a name a file may not have, or a folder path
    /// that is no path.
    fn named(arguments: &Map<String, Value>) -> Result<Self, String> {
        let content = arguments
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let hash = tilo_actions::content_hash(&content)
            .ok_or_else(|| {
                format!("`{content}` names no content: give the file's `tilo.content:<reference>`")
            })?
            .to_owned();
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned();
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
            return Err(format!("{name:?} is no file's name"));
        }
        let dir = folder(
            arguments
                .get("dir")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )?;
        Ok(Self {
            content,
            hash,
            dir,
            name,
        })
    }
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

/// Where a workspace's own files live on the shelf their content is kept on.
fn files_scope(workspace: WorkspaceId) -> ContentScope {
    ContentScope {
        workspace: workspace.to_string(),
        app: FILES_APP.to_owned(),
    }
}

/// Where content put into a workspace lives before any record references it.
fn put_scope(workspace: WorkspaceId) -> ContentScope {
    ContentScope {
        workspace: workspace.to_string(),
        app: CONTENT_PUT_SCOPE.to_owned(),
    }
}

/// The route the workspace's tree travels as, whose keys seal its files.
fn tree_route(workspace: WorkspaceId) -> DocumentRoute {
    DocumentRoute {
        app_id: FILES_APP.to_owned(),
        cluster_id: workspace.to_string(),
        document: tree::document_id(),
    }
}

/// The files a `files#open` names, put where this service keeps the workspace's tree.
pub struct OpenedFiles {
    workspaces: Workspaces,
    shelf: Arc<dyn ContentShelf>,
}

impl OpenedFiles {
    /// Puts into the trees `workspaces` holds, their files' bytes read and kept on `shelf`.
    #[must_use]
    pub fn new(workspaces: Workspaces, shelf: Arc<dyn ContentShelf>) -> Self {
        Self { workspaces, shelf }
    }

    /// Put what `arguments` name into `workspace`'s tree, as one change of it, answered with
    /// where the file is or how many went in.
    ///
    /// `confirm` is asked once every file is named and before the tree is touched: whether the
    /// workspace still lets the caller write its files.
    ///
    /// The change is made in `call`'s context, keeping its request ([`Self::kept_call`]).
    ///
    /// # Errors
    /// A content reference the workspace does not hold, a name or folder the tree refuses, keys
    /// the workspace's files need and this device cannot read, a call that does not sign here, a
    /// grant taken back while the content was read, or a change the workspace did not take in
    /// time.
    pub fn put(
        &self,
        workspace: WorkspaceId,
        arguments: &Map<String, Value>,
        call: Option<FilesPutCall<'_>>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        let acting = call
            .map(|call| self.kept_call(workspace, call))
            .transpose()?;
        let many = arguments.contains_key("files");
        let read = |hash: &str| self.read(workspace, hash);
        let puttings = Putting::all(arguments, &read)?;
        let sealing = self.sealing(workspace)?;
        let files = self.named_for_the_tree(workspace, sealing.as_ref(), puttings)?;
        self.same_keys(workspace, sealing.as_ref())?;
        if !confirm() {
            return Err("the workspace no longer lets this caller write its files".to_owned());
        }
        let mtime = tilo_clock::unix_now_ms();
        // Every file goes in as one change of the tree: one write per record
        // it touched, however many files went into it.
        let to_put: Vec<FileToPut<'_>> = files
            .iter()
            .map(|(putting, manifest, size)| FileToPut {
                to: &putting.dir,
                name: &putting.name,
                hash: manifest,
                size: *size,
                mtime,
            })
            .collect();
        tilo_rt::app()
            .block_on(
                self.workspaces
                    .puts()
                    .change(workspace, acting, &mut |tree| {
                        let placed = tree.put_files(&to_put)?;
                        // A folder's answer names how many files it put, not each: the caller sent the
                        // list and knows where each file is.
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
                    }),
            )
            .map_err(|error| error.to_string())?
    }

    /// `call`'s context, keeping its request as every replica of the tree checks it: as its
    /// caller signed it, or, for an agent's call signed otherwise, as this device signs it as the
    /// one that carries it out for the agent's person, with how the agent was allowed
    /// ([`Workspaces::carried_call`]). A call kept as its caller signed it keeps its argument, so
    /// a replica checks the content and names the call put against the signature.
    ///
    /// # Errors
    /// The call does not sign here.
    fn kept_call(
        &self,
        workspace: WorkspaceId,
        FilesPutCall {
            mut context,
            request,
        }: FilesPutCall<'_>,
    ) -> Result<ActionContext, String> {
        let acting = context
            .invoker
            .as_ref()
            .map(|invoker| invoker.acting.clone());
        // An agent's put is the call it signed: this device signs none for it.
        if let Some(acting) = acting.as_ref().filter(|acting| acting.is_agent())
            && request
                .signed
                .as_deref()
                .is_none_or(|signed| !tilo_server_protocol::invocation::signs_for(acting, signed))
        {
            return Err(tilo_broker_core::services::KEYLESS_AGENT.to_owned());
        }
        let kept = tilo_rt::app()
            .block_on(self.workspaces.carried_call(
                &workspace.to_string(),
                request,
                acting.as_ref(),
                &[],
            ))
            .map_err(|error| error.to_string())??;
        context.signed = kept.map(|(invocation, mut call)| {
            context.source_invocation = Some(invocation);
            if Some(&call.signed) == request.signed.as_deref() {
                call.argument =
                    tilo_actions::remote::SignedCall::of(request).and_then(|own| own.argument);
            }
            Box::new(call)
        });
        Ok(context)
    }

    /// What the folder at `folder` of `workspace`'s tree holds, as the workspace has it now: its
    /// entries, and with `below` every entry beneath them too, each by its path. Read from the
    /// tree a put changes, so a file put is listed once its put answered.
    ///
    /// # Errors
    /// A path that leaves the workspace or names no folder of its tree, or this device does not
    /// hold the workspace.
    pub fn entries(
        &self,
        workspace: WorkspaceId,
        folder: &str,
        below: bool,
    ) -> Result<Vec<FilesEntry>, String> {
        let folder = super::paths::path_of(folder)?;
        let mut entries = Vec::new();
        tilo_rt::app()
            .block_on(self.workspaces.puts().read(workspace, &mut |tree| {
                entries.clear();
                let start = tree
                    .resolve_path_ino(&folder)
                    .ok_or_else(|| format!("{folder:?} is no folder of this workspace"))?;
                let mut folders = vec![(start, folder.clone())];
                while let Some((ino, path)) = folders.pop() {
                    let children = tree
                        .readdir(ino)
                        .ok_or_else(|| format!("{path:?} is no folder of this workspace"))?;
                    for (child, kind, name) in children {
                        // A name that is not UTF-8 is no path a listing offers, and the tree lists
                        // its dot entries as any directory does.
                        let Ok(name) = String::from_utf8(name.as_bytes().to_vec()) else {
                            continue;
                        };
                        if name == "." || name == ".." {
                            continue;
                        }
                        let path = if path.is_empty() {
                            name
                        } else {
                            format!("{path}/{name}")
                        };
                        let is_folder = kind == tilo_fs::node::FileType::Directory;
                        if is_folder && below {
                            folders.push((child, path.clone()));
                        }
                        entries.push(FilesEntry {
                            path,
                            folder: is_folder,
                        });
                    }
                }
                Ok(Value::Null)
            }))
            .map_err(|error| error.to_string())??;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    /// The bytes named `hash` this workspace keeps: content put into it, or a file its tree
    /// already names.
    fn read(&self, workspace: WorkspaceId, hash: &str) -> Option<Vec<u8>> {
        let files = files_scope(workspace);
        let put = put_scope(workspace);
        self.shelf
            .read(&put, hash)
            .or_else(|| self.shelf.read(&files, hash))
    }

    /// The scope `hash` is kept under, and how long it is there.
    fn source(&self, workspace: WorkspaceId, hash: &str) -> Option<(ContentScope, u64)> {
        let files = files_scope(workspace);
        let put = put_scope(workspace);
        self.shelf
            .size(&put, hash)
            .map(|size| (put, size))
            .or_else(|| self.shelf.size(&files, hash).map(|size| (files, size)))
    }

    /// The keys the workspace's files are sealed with, `None` where this device takes part in
    /// no key group of the workspace and its files are kept in the clear. A put opens the key
    /// group its files are about to be sealed in.
    fn sealing(&self, workspace: WorkspaceId) -> Result<Option<key_group::ContentKeys>, String> {
        self.keys(workspace, true)
    }

    /// [`Self::sealing`], opening the key group for writes about to start when `create`, and
    /// otherwise only reading an existing one, as a read of the files does.
    fn keys(
        &self,
        workspace: WorkspaceId,
        create: bool,
    ) -> Result<Option<key_group::ContentKeys>, String> {
        match tilo_rt::app()
            .block_on(
                self.workspaces
                    .own_route_keys(&tree_route(workspace), create),
            )
            .map_err(|error| error.to_string())?
        {
            Some(answer) => answer.map(Some),
            None => Ok(None),
        }
    }

    /// Why the keys changed while the files were saved, or `None` when they are the ones the
    /// files were sealed with: the same ask the put made, made again.
    fn same_keys(
        &self,
        workspace: WorkspaceId,
        used: Option<&key_group::ContentKeys>,
    ) -> Result<(), String> {
        let current = self.sealing(workspace)?;
        let generation = |keys: &key_group::ContentKeys| (keys.workspace(), keys.epoch());
        if used.map(generation) != current.as_ref().map(generation) {
            return Err("file access changed while it was saved".to_owned());
        }
        Ok(())
    }

    /// Each file of `puttings` with the manifest the tree names its content by and its length:
    /// what the workspace does not hold of a manifest is kept. Every file is named before the
    /// tree is touched, so one the workspace holds no content for refuses the whole open.
    fn named_for_the_tree(
        &self,
        workspace: WorkspaceId,
        sealing: Option<&key_group::ContentKeys>,
        puttings: Vec<Putting>,
    ) -> Result<Vec<(Putting, String, u64)>, String> {
        let mut kept: BTreeMap<String, (u64, String)> = BTreeMap::new();
        let mut files = Vec::with_capacity(puttings.len());
        for putting in puttings {
            let (size, manifest) = if let Some(named) = kept.get(&putting.hash) {
                named.clone()
            } else {
                let named = self.named_content(workspace, sealing, &putting)?;
                kept.insert(putting.hash.clone(), named.clone());
                named
            };
            files.push((putting, manifest, size));
        }
        Ok(files)
    }

    /// The length of the content `putting` names and the manifest the tree names it by, kept on
    /// the shelf the workspace's files live on.
    fn named_content(
        &self,
        workspace: WorkspaceId,
        sealing: Option<&key_group::ContentKeys>,
        putting: &Putting,
    ) -> Result<(u64, String), String> {
        let (scope, size) = self
            .source(workspace, &putting.hash)
            .ok_or_else(|| format!("this workspace holds no {}", putting.content))?;
        let encoded = self.manifest(workspace, &scope, putting, size, sealing)?;
        let address = hex::encode(blake3::hash(&encoded).as_bytes());
        let files = files_scope(workspace);
        if !self.shelf.has(&files, &address) {
            self.shelf.keep(&files, &address, &encoded)?;
        }
        Ok((size, address))
    }

    /// The manifest of the content `putting` names, `size` bytes long, as a file of the tree is
    /// named: chunked, each chunk the shelf holds none of kept as its end is known — a file of a
    /// gigabyte costs what one chunk does in memory, not the file — and the manifest listing
    /// them answered encoded.
    fn manifest(
        &self,
        workspace: WorkspaceId,
        scope: &ContentScope,
        putting: &Putting,
        size: u64,
        sealing: Option<&key_group::ContentKeys>,
    ) -> Result<Vec<u8>, String> {
        let files = files_scope(workspace);
        let artifact = collab_types::file_content_artifact(workspace);
        let mut failed: Option<String> = None;
        let mut chunks: Vec<ChunkRef> = Vec::new();
        let mut overhead: Option<u32> = None;
        let mut keep = |part: &ChunkRef, clear: &[u8]| {
            if failed.is_some() {
                return;
            }
            let (hash, address, stored) = match sealing {
                Some(keys) => {
                    let (_, cipher) = match keys.seal_content(
                        artifact,
                        collab_types::ContentRetention::Delta,
                        clear,
                    ) {
                        Ok(sealed) => sealed,
                        Err(error) => {
                            failed = Some(error.to_string());
                            return;
                        }
                    };
                    let Ok(extra) = u32::try_from(cipher.len().saturating_sub(clear.len())) else {
                        failed = Some("a sealed chunk grew past what a file may keep".to_owned());
                        return;
                    };
                    if *overhead.get_or_insert(extra) != extra {
                        failed = Some("inconsistent sealed file chunk length".to_owned());
                        return;
                    }
                    let address = *blake3::hash(&cipher).as_bytes();
                    (hex::encode(address), address, cipher)
                }
                None => (hex::encode(part.hash), part.hash, clear.to_vec()),
            };
            if !self.shelf.has(&files, &hash) {
                match self.shelf.keep(&files, &hash, &stored) {
                    Ok(()) => {}
                    Err(error) => {
                        failed = Some(error);
                        return;
                    }
                }
            }
            chunks.push(ChunkRef {
                hash: address,
                len: part.len,
            });
        };
        let mut stream = Streamed::new(size, None);
        let mut offset = 0_u64;
        while offset < size {
            let len = size.saturating_sub(offset).min(PIECE);
            let len = usize::try_from(len).unwrap_or(usize::MAX);
            let piece = self
                .shelf
                .read_range(scope, &putting.hash, offset, len)
                .filter(|piece| !piece.is_empty())
                .ok_or_else(|| format!("this workspace holds no {}", putting.content))?;
            stream
                .push(&piece, &mut keep)
                .map_err(|error| error.to_string())?;
            offset = offset.saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
        }
        let (manifest, _) = stream
            .finish(&mut keep)
            .map_err(|error| error.to_string())?;
        if let Some(failed) = failed {
            return Err(failed);
        }
        Ok(match (sealing, overhead) {
            (Some(keys), overhead) => Manifest::new_sealed(
                manifest.tier(),
                chunks,
                SealedChunks {
                    group: *keys.workspace().as_uuid().as_bytes(),
                    epoch: keys.epoch(),
                    overhead: overhead.unwrap_or_default(),
                },
            ),
            (None, _) => manifest,
        }
        .encode())
    }
}

#[path = "changes.rs"]
mod changes;
#[path = "reads.rs"]
mod reads;

#[cfg(test)]
#[path = "opened_tests.rs"]
mod tests;
