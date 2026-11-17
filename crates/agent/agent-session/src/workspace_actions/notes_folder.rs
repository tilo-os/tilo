//! A workspace's notes as `.md` files in its folder.
//!
//! Every note of the workspace's notebook is also a file, `Notes/<title>.md`, in the
//! workspace's folder (its Files record, the tree every member mounts), so an agent, a
//! script or any program reads and writes notes as plain files. The note record is the
//! source of truth and the file its projection; there is one copy of each, kept in step:
//!
//! - A note made or edited in Notes is written into its file, as the host's own change of
//!   the tree.
//! - A write to the file (through a computer's mounted folder, an agent, another program) is
//!   applied to the note as Notes' own `write-note` action, as the change of whoever wrote
//!   the file: the person whose device signed the file's version, and the agent session
//!   that wrote for them. Notes' History names them.
//! - Deleting the file deletes the note with Notes' own `delete`, which Notes undoes like
//!   any delete; a note brought back is put back in the folder, its file restored from the
//!   trash. A note deleted in Notes takes its file to the folder's trash.
//!
//! The file's node in the tree names the note it projects and what both last read as
//! ([`Namespace::set_projection`]): a file that reads otherwise was written in the folder,
//! a note that reads otherwise was changed in Notes, and the host's own writes read as
//! agreed, so nothing it writes comes back to it as a change. When both moved, the note
//! wins and the file is written from it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use fs_protocol::files::TreeChunk;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_fs::namespace::{Namespace, ROOT_INO};
use tilo_fs::tree_document::{FILES_APP, document_id as tree_document};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::WorkspaceId;

use tilo_lock::MutexRecover as _;

use super::tree_put::HeldChunks;
use super::{Needs, Worker};
use crate::member::actions::record_provider;

/// The app whose workspace record the notebook is.
pub use ui::apps::NOTES as NOTES_APP;
/// The folder of the workspace's folder the notes are files in.
pub const FOLDER: &str = "Notes";
/// What a projection's agreement says once the file's note was deleted through it: the
/// file stays in the trash with the note, and comes back if the note does.
const GONE: &str = "gone";

/// The source a note's file names: `so.tilo.notes:<note id>`.
fn source_of(page: &str) -> String {
    format!("{NOTES_APP}:{page}")
}

fn page_of_source(source: &str) -> Option<&str> {
    source.strip_prefix(NOTES_APP)?.strip_prefix(':')
}

/// What the projection agreement is kept as: the BLAKE3 of the Markdown.
fn agreed_of(markdown: &[u8]) -> String {
    blake3::hash(markdown).to_hex().to_string()
}

/// The file name a note titled `title` has: its title, with what a name may not hold
/// replaced, and `.md`.
fn file_name(title: &str) -> String {
    let title: String = title
        .trim()
        .chars()
        .map(|c| if c == '/' || c == '\0' { '-' } else { c })
        .collect();
    let title = if title.is_empty() || title == "." || title == ".." {
        "Untitled".to_owned()
    } else {
        title
    };
    format!("{title}.md")
}

/// Whether `name` is `wanted`, or `wanted` made free beside a same-named file (`Plan (2).md`).
fn names_the_same(name: &str, wanted: &str) -> bool {
    if name == wanted {
        return true;
    }
    let (Some(stem), Some(wanted_stem)) = (name.strip_suffix(".md"), wanted.strip_suffix(".md"))
    else {
        return false;
    };
    stem.strip_prefix(wanted_stem)
        .and_then(|rest| rest.strip_prefix(" ("))
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// One note as its file shows it.
#[derive(Clone)]
pub(super) struct Note {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) markdown: Vec<u8>,
}

/// The notebook's pages in `doc`, by the ids they are stored under.
pub(super) fn page_keys(doc: &AutoCommit) -> Vec<String> {
    use automerge::ReadDoc as _;
    notes::Notebook::paths()
        .pages
        .object(doc)
        .map(|pages| doc.keys(&pages).collect())
        .unwrap_or_default()
}

/// The pages of the notebook `doc` that changed between its heads `before` and `after`:
/// made, deleted, renamed, moved or written. Only they are read again, so a pass costs the
/// notes that changed, never the notebook.
pub(super) fn pages_changed(
    doc: &mut AutoCommit,
    before: &[automerge::ChangeHash],
    after: &[automerge::ChangeHash],
) -> BTreeSet<String> {
    let Some(pages) = notes::Notebook::paths().pages.object(doc) else {
        return BTreeSet::new();
    };
    doc.diff(before, after)
        .iter()
        .filter_map(|patch| {
            if patch.obj == pages {
                return match &patch.action {
                    automerge::PatchAction::PutMap { key, .. }
                    | automerge::PatchAction::DeleteMap { key } => Some(key.clone()),
                    _ => None,
                };
            }
            patch.path.iter().find_map(|(parent, prop)| match prop {
                automerge::Prop::Map(key) if *parent == pages => Some(key.clone()),
                _ => None,
            })
        })
        .collect()
}

/// The note stored under `id` in the notebook `doc`, with its body as Markdown; `None` once
/// it is gone.
pub(super) fn note(doc: &AutoCommit, id: &str) -> Option<Note> {
    let page = notes::notebook::page_of(&ui::tilo_resource::Id::from(id))?;
    let object = notes::Notebook::paths()
        .pages
        .item_path(&notes::notebook::page_id(page))
        .object(doc)?;
    // A title is kept as text, or as a string a field edit wrote whole.
    let title = match automerge::ReadDoc::get(doc, &object, "title") {
        Ok(Some((value, at))) => {
            <String as ui::tilo_resource::Model>::load(doc, &value, &at).unwrap_or_default()
        }
        _ => String::new(),
    };
    Some(Note {
        id: id.to_owned(),
        title,
        markdown: body_markdown(doc, page).unwrap_or_default(),
    })
}

/// Note `page`'s body in `doc` as Markdown, written by the same codec the folder writes
/// its `.md` files with, so the two read alike.
fn body_markdown(doc: &AutoCommit, page: notes::PageId) -> Option<Vec<u8>> {
    let body = notes::notebook::body_object(doc, page)?;
    let containers = tilo_doc::model::open(doc, &body).ok()?;
    tilo_doc_codecs::provider_for_ext("md")?
        .export(
            tilo_doc::DocRef {
                doc,
                c: &containers,
            },
            &tilo_content::NoContent,
        )
        .ok()
}

/// A change to a note the folder asked for, made as Notes' own action by `by`.
pub(super) struct NoteEdit {
    pub(super) page: String,
    pub(super) action: &'static str,
    pub(super) arguments: Map<String, Value>,
    pub(super) by: Caller,
    /// The node whose agreement reads `agreed` once the edit is made.
    pub(super) then: (String, String),
}

/// Who wrote the newest version of the file at node `id` of the tree: the person
/// whose device signed it, and the agent or program that wrote for them. `None` when the
/// tree records no version of it.
fn writer(tree: &Namespace, id: &str) -> Option<Caller> {
    let record = tree.document_id_for_id(id)?;
    let (version, attribution) = tree.latest_version(record)?;
    let caller = Caller::authenticated(version.actor.person.0, version.actor.device.0);
    Some(match attribution.map(|attributed| attributed.attribution) {
        Some(fs_protocol::FileAttributionV1 {
            writer: fs_protocol::AttributedWriter::Agent { agent, session },
            label,
            ..
        }) => {
            let caller = caller.delegated_to(agent).named(&label).in_new_execution();
            match session {
                Some(session) => caller.in_agent_session(session),
                None => caller,
            }
        }
        _ => caller,
    })
}

/// The file of each note in the folder: the node projecting it, by the note's id. Read
/// from the tree once, then kept as the tree changes.
pub(super) type Files = HashMap<String, String>;

/// Every note file the tree `tree` holds, live or in the trash, by the note it projects.
pub(super) fn files_of(tree: &Namespace) -> Files {
    tree.projected_nodes()
        .into_iter()
        .filter_map(|id| {
            let (source, _) = tree.projection(&id)?;
            Some((page_of_source(&source)?.to_owned(), id))
        })
        .collect()
}

/// Bring the files of the notes in `dirty` and the notes into step, as far as the tree alone
/// can: files written from notes that moved, files of deleted notes trashed, deleted notes'
/// files restored when the note came back, and new notes' files made. `dirty` holds each
/// note whose note or file changed, as it reads now (`None` once deleted); nothing else is
/// read. What the folder asks of the notes is answered as edits to make with Notes' own
/// actions.
pub(super) fn reconcile(
    tree: &mut Namespace,
    files: &mut Files,
    dirty: &BTreeMap<String, Option<Note>>,
    mtime: u64,
) -> Vec<NoteEdit> {
    let mut edits = Vec::new();
    for (page, note) in dirty {
        let projected = files.get(page).and_then(|id| {
            let (source, agreed) = tree.projection(id)?;
            (page_of_source(&source) == Some(page.as_str())).then(|| (id.clone(), source, agreed))
        });
        let Some((id, source, agreed)) = projected else {
            files.remove(page);
            if let Some(note) = note
                && let Some(id) = make_file(tree, note, mtime)
            {
                files.insert(page.clone(), id);
            }
            continue;
        };
        let trashed = tree.is_directly_trashed(&id);
        match (note, trashed) {
            // The note was deleted in Notes: its file goes to the trash.
            (None, false) => {
                if let Some(ino) = tree.inode_for_node(&id)
                    && tree.remove(ino)
                {
                    tree.set_projection(&id, &source, GONE);
                }
            }
            (None, true) => {}
            // The note came back (its delete undone): so does its file.
            (Some(note), true) if agreed == GONE => {
                if tree.restore(&id) {
                    write_file(tree, &id, note, mtime);
                }
            }
            // The file was deleted in the folder: so is the note, as whoever deleted it.
            (Some(note), true) => {
                if let Some(by) = writer(tree, &id) {
                    let mut arguments = Map::new();
                    arguments.insert("page".into(), json!(note.id));
                    edits.push(NoteEdit {
                        page: note.id.clone(),
                        action: "delete",
                        arguments,
                        by,
                        then: (id.clone(), GONE.to_owned()),
                    });
                }
            }
            (Some(note), false) => {
                edits.extend(follow(tree, &id, note, &agreed, mtime));
            }
        }
    }
    edits
}

/// A live note file `id` and its note: written from the note when the note moved, applied
/// to the note when only the file did, and named for the note's title.
fn follow(
    tree: &mut Namespace,
    id: &str,
    note: &Note,
    agreed: &str,
    mtime: u64,
) -> Option<NoteEdit> {
    let ino = tree.inode_for_node(id)?;
    let file = tree.export_document(ino, &tilo_content::NoContent)?;
    rename_for(tree, id, note, mtime);
    let file_agreed = agreed_of(&file);
    let note_agreed = agreed_of(&note.markdown);
    tracing::trace!(
        page = %note.id,
        agreed,
        file = %file_agreed,
        note = %note_agreed,
        "notes folder: a note and its file compared"
    );
    if file_agreed == note_agreed {
        if agreed != note_agreed {
            tree.set_projection(id, &source_of(&note.id), &note_agreed);
        }
        return None;
    }
    if note_agreed == agreed {
        // Only the file moved: the note is written as it says, by whoever wrote it.
        let Some(by) = writer(tree, id) else {
            tracing::debug!(
                page = %note.id,
                record = ?tree.document_id_for_id(id),
                "notes folder: a note's file changed with no version saying who wrote it"
            );
            return None;
        };
        let markdown = String::from_utf8_lossy(&file).into_owned();
        let mut arguments = Map::new();
        arguments.insert("page".into(), json!(note.id));
        arguments.insert("markdown".into(), json!(markdown));
        return Some(NoteEdit {
            page: note.id.clone(),
            action: "write-note",
            arguments,
            by,
            then: (id.to_owned(), file_agreed),
        });
    }
    // The note moved (and the file too, perhaps): the note is the source of truth.
    tracing::debug!(
        page = %note.id,
        agreed,
        file = %file_agreed,
        note = %note_agreed,
        "notes folder: a note's file is written from the note"
    );
    write_file(tree, id, note, mtime);
    None
}

/// Write note `note` into its file `id`, and record that both read the same.
fn write_file(tree: &mut Namespace, id: &str, note: &Note, mtime: u64) {
    let Some(ino) = tree.inode_for_node(id) else {
        return;
    };
    let content = tilo_content::ContentStore::memory();
    if tree.import_document(ino, &note.markdown, mtime, &content) {
        let written = tree
            .export_document(ino, &tilo_content::NoContent)
            .unwrap_or_else(|| note.markdown.clone());
        // What the file reads now is what the note reads: a codec that writes the file
        // otherwise than the note's export agrees with it all the same.
        let agreed = if written == note.markdown {
            agreed_of(&written)
        } else {
            agreed_of(&note.markdown)
        };
        tree.set_projection(id, &source_of(&note.id), &agreed);
    }
}

/// Name note file `id` for its note's title, when it is named otherwise.
fn rename_for(tree: &mut Namespace, id: &str, note: &Note, mtime: u64) {
    let wanted = file_name(&note.title);
    let Some((parent, name)) = tree.node_parent_name(id) else {
        return;
    };
    let name = String::from_utf8_lossy(name.as_bytes()).into_owned();
    if names_the_same(&name, &wanted) {
        return;
    }
    let Some(parent_ino) = tree
        .inode_for_node(&parent)
        .or_else(|| (parent == fs_protocol::tree::ROOT_ID).then_some(ROOT_INO))
    else {
        return;
    };
    tracing::debug!(%name, %wanted, "notes folder: a note's file is renamed for its title");
    let free = free_name(tree, &parent, &wanted);
    tree.rename(
        parent_ino,
        name.as_bytes(),
        parent_ino,
        free.as_bytes(),
        mtime,
    );
}

/// `wanted`, or `wanted` with a number when the folder `parent` has a file of that name.
fn free_name(tree: &Namespace, parent: &str, wanted: &str) -> String {
    if tree.child_id_named(parent, wanted).is_none() {
        return wanted.to_owned();
    }
    let stem = wanted.strip_suffix(".md").unwrap_or(wanted);
    // A folder holds fewer files than this, so one of these names is free.
    (2_u32..=u32::MAX)
        .map(|n| format!("{stem} ({n}).md"))
        .find(|name| tree.child_id_named(parent, name).is_none())
        .unwrap_or_else(|| wanted.to_owned())
}

/// The folder notes are files in, made when the workspace has none.
fn folder(tree: &mut Namespace, mtime: u64) -> Option<(String, u64)> {
    let id = match tree.child_id_named(fs_protocol::tree::ROOT_ID, FOLDER) {
        Some(id) if tree.node_is_dir(&id) == Some(true) => id,
        Some(_) => return None,
        None => {
            let ino = tree.create_dir(ROOT_INO, FOLDER, mtime)?;
            tree.id_for_ino(ino)?.to_owned()
        }
    };
    let ino = tree.inode_for_node(&id)?;
    Some((id, ino))
}

/// Make note `note`'s file in the notes folder; the file's node.
fn make_file(tree: &mut Namespace, note: &Note, mtime: u64) -> Option<String> {
    tracing::debug!(page = %note.id, "notes folder: a note's file is made");
    let (folder, folder_ino) = folder(tree, mtime)?;
    let name = free_name(tree, &folder, &file_name(&note.title));
    let ino = tree.create_document(folder_ino, &name, "md", mtime)?;
    let id = tree.id_for_ino(ino).map(str::to_owned)?;
    write_file(tree, &id, note, mtime);
    Some(id)
}

/// How long a failed pass over a workspace's notes waits before it is tried again, the
/// first time; each failure after doubles it, up to [`RETRY_MOST`].
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(2);
/// The longest a failed pass waits before it is tried again.
const RETRY_MOST: std::time::Duration = std::time::Duration::from_secs(300);

/// How long the pass that failed `failures` times in a row waits before it is tried again.
fn retry_after(failures: u32) -> std::time::Duration {
    RETRY_FIRST
        .saturating_mul(1_u32 << failures.saturating_sub(1).min(16))
        .min(RETRY_MOST)
}

impl Worker {
    /// Run a pass over `workspace_id`'s notes and their files: one that fails (the
    /// workspace not open yet after a restart, a note's edit refused) is tried again after
    /// a wait, never left until the workspace's next write.
    pub(super) fn notes_pass(&mut self, workspace_id: WorkspaceId) {
        self.shared.projecting.lock_recover().remove(&workspace_id);
        let Err(error) = self.project_notes(workspace_id) else {
            self.notes_failures.remove(&workspace_id);
            return;
        };
        let failures = self.notes_failures.entry(workspace_id).or_default();
        *failures = failures.saturating_add(1);
        let wait = retry_after(*failures);
        tracing::warn!(%workspace_id, %error, failures = *failures, retry_in = ?wait,
            "workspace actions: the notes and their files were not brought into step");
        let shared = std::sync::Arc::clone(&self.shared);
        let retried = std::thread::Builder::new()
            .name("notes-retry".into())
            .spawn(move || {
                std::thread::sleep(wait);
                shared.queue_notes_pass(workspace_id);
            });
        if let Err(error) = retried {
            tracing::warn!(%workspace_id, %error, "workspace actions: a failed notes pass is not tried again");
        }
    }

    /// Make `edits` with Notes' own actions, each as its writer, and record what each
    /// note and its file agree on once it is made. Answers why, when one was not made.
    fn apply_note_edits(
        &mut self,
        workspace_id: WorkspaceId,
        notes_route: &DocumentRoute,
        tree: &mut Namespace,
        edits: Vec<NoteEdit>,
    ) -> Result<(), String> {
        let mut failed = None;
        for edit in edits {
            let request = InvokeRequest {
                invocation: InvocationId::new(),
                target: RemoteActionTarget {
                    provider: record_provider(notes_route),
                    address: ActionAddress::new("", edit.action),
                },
                argument: InvokeArgument::Json(edit.arguments),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
            };
            let performed = self.perform(workspace_id, &edit.by, &request);
            let (outcome, _) = self.saved(workspace_id, performed);
            match outcome {
                Ok(_) => {
                    let (node, agreed) = edit.then;
                    if let Some((source, _)) = tree.projection(&node) {
                        tree.set_projection(&node, &source, &agreed);
                    }
                }
                Err(error) => {
                    let error = match &error {
                        crate::member::actions::Outcome::Rejected(error) => error.to_string(),
                        crate::member::actions::Outcome::Failed(error) => error.message.clone(),
                    };
                    tracing::warn!(page = %edit.page, action = edit.action, %error,
                        "a note file's change was not applied to its note");
                    failed = Some(format!("{} of note {}: {error}", edit.action, edit.page));
                }
            }
        }
        failed.map_or(Ok(()), Err)
    }

    /// Bring `workspace_id`'s notes and their files in its folder into step
    /// ([module docs](self)). Run whenever either record of the workspace takes a write.
    ///
    /// The notebook and the tree are kept between passes ([`Followed`]): a pass merges
    /// only the changes since the last, and reads, renders and writes only the notes and
    /// files they touched, so one note's edit costs that note in a notebook of thousands.
    pub(super) fn project_notes(&mut self, workspace_id: WorkspaceId) -> Result<(), String> {
        let tree_name = tree_document();
        let notes_route = DocumentRoute {
            app_id: NOTES_APP.to_owned(),
            cluster_id: workspace_id.to_string(),
            document: String::new(),
        };
        self.refresh(
            workspace_id,
            Needs::Act {
                app_id: NOTES_APP,
                record: "",
            },
        )
        .map_err(|error| error.to_string())?;
        let state = self
            .refresh(
                workspace_id,
                Needs::Act {
                    app_id: FILES_APP,
                    record: &tree_name,
                },
            )
            .map_err(|error| error.to_string())?;
        let workspace = workspace_id.to_string();
        // What failed part way is read whole again by the next pass.
        let held = self.notes_followed.remove(&workspace_id);
        let caught_up = if let Some(held) = held {
            self.catch_up(held, &state, &notes_route, workspace_id)?
        } else {
            None
        };
        let (mut followed, dirty) = if let Some(caught_up) = caught_up {
            caught_up
        } else {
            // A workspace that has no notebook yet has no notes to show; one whose folder
            // nobody wrote yet gets one when the first note is made.
            let Ok((notebook, notebook_heads)) = state.document(&notes_route) else {
                return Ok(());
            };
            let (tree, chunks) = self
                .tree_with_heads(workspace_id, &workspace)
                .map_err(outcome_text)?;
            let Some(read) = Followed::read(&notebook, notebook_heads, tree, chunks)? else {
                return Ok(());
            };
            read
        };
        let dirty: BTreeMap<String, Option<Note>> = dirty
            .into_iter()
            .map(|page| {
                let note = note(&followed.notebook, &page);
                (page, note)
            })
            .collect();
        let mtime = tilo_clock::unix_now_ns() / 1_000_000;
        // What loading the tree settled of its own (ids it lacked) is not this pass's
        // change: the tree is written only where the notes asked something of it.
        let loaded = followed.tree.record_heads();
        let edits = reconcile(&mut followed.tree, &mut followed.files, &dirty, mtime);
        tracing::debug!(
            %workspace_id,
            notes = dirty.len(),
            edits = edits.len(),
            "notes folder: a pass over the notes and their files that changed"
        );
        let applied = self.apply_note_edits(workspace_id, &notes_route, &mut followed.tree, edits);
        if followed.chunks.is_some() && followed.tree.record_heads() == loaded {
            self.keep_followed(workspace_id, followed);
            return applied;
        }
        // A tree founded now is written whole; one read is written where the pass changed it.
        let known = if followed.chunks.is_some() {
            loaded
        } else {
            BTreeMap::new()
        };
        let writes = followed.tree.document_writes(&known);
        let held: Option<BTreeSet<TreeChunk>> = followed
            .chunks
            .as_ref()
            .map(|chunks| chunks.keys().copied().collect());
        self.commit_tree_writes(workspace_id, &workspace, held.as_ref(), writes)?;
        // Each record of the tree now stands where this pass left it: the next pass merges
        // only what others write after.
        followed.chunks = Some(
            followed
                .tree
                .record_heads()
                .into_iter()
                .map(|(chunk, heads)| (chunk, heads.iter().map(|head| head.0).collect()))
                .collect(),
        );
        self.keep_followed(workspace_id, followed);
        applied
    }

    /// Merge what the notebook and the tree took since `followed` read them: the notes whose
    /// note or file changed. The tree is followed record by record, a chunk the tree's own
    /// record lists anew read whole. `None` when the notebook is no longer where it was read
    /// from (started anew), or a tree appeared where there was none, to be read whole.
    fn catch_up(
        &mut self,
        mut followed: Followed,
        state: &super::WorkspaceState,
        notes_route: &DocumentRoute,
        workspace_id: WorkspaceId,
    ) -> Result<Option<(Followed, BTreeSet<String>)>, String> {
        let mut dirty = BTreeSet::new();
        match state.update(notes_route, Some(&followed.notebook_heads))? {
            None => {}
            Some((DocumentMutation::Incremental(change), heads)) => {
                let before = followed.notebook.get_heads();
                tilo_automerge::ApplyIncoming::load_incoming(&mut followed.notebook, &change)
                    .map_err(|error| error.to_string())?;
                let after = followed.notebook.get_heads();
                dirty.extend(pages_changed(&mut followed.notebook, &before, &after));
                followed.notebook_heads = heads;
            }
            Some((DocumentMutation::Snapshot(_), _)) => return Ok(None),
        }
        let workspace = workspace_id.to_string();
        let Some(chunks) = followed.chunks.as_mut() else {
            if self
                .record_here(workspace_id, &Self::tree_route(&workspace), false)
                .map_err(outcome_text)?
                .is_some()
            {
                return Ok(None);
            }
            return Ok(Some((followed, dirty)));
        };
        // The tree's own record first: it lists the chunks there are.
        let mut records = Vec::new();
        if let Some(heads) = chunks.get(&0)
            && let Some((mutation, heads)) =
                state.update(&Self::tree_route(&workspace), Some(heads))?
        {
            records.push((0, mutation_bytes(mutation)));
            chunks.insert(0, heads);
        }
        let mut touched = followed
            .tree
            .take_records_touching(std::mem::take(&mut records))
            .map_err(|error| error.to_string())?;
        for chunk in followed.tree.listed_chunks() {
            let route = Self::tree_chunk_route(&workspace, chunk);
            match chunks.get(&chunk) {
                Some(heads) => {
                    if let Some((mutation, heads)) = state.update(&route, Some(heads))? {
                        records.push((chunk, mutation_bytes(mutation)));
                        chunks.insert(chunk, heads);
                    }
                }
                None => {
                    if let Some((bytes, heads)) = self
                        .saved_here(workspace_id, &route, false)
                        .map_err(outcome_text)?
                    {
                        records.push((chunk, bytes));
                        chunks.insert(chunk, heads);
                    }
                }
            }
        }
        touched.extend(
            followed
                .tree
                .take_records_touching(records)
                .map_err(|error| error.to_string())?,
        );
        for node in touched {
            let Some((source, _)) = followed.tree.projection(&node) else {
                continue;
            };
            if let Some(page) = page_of_source(&source) {
                followed.files.insert(page.to_owned(), node);
                dirty.insert(page.to_owned());
            }
        }
        Ok(Some((followed, dirty)))
    }
}

/// The bytes a record's update carries: its changes, or the record whole; either merges.
fn mutation_bytes(mutation: DocumentMutation) -> Vec<u8> {
    match mutation {
        DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes) => bytes,
    }
}

/// An action's outcome as the notes folder reports it.
fn outcome_text(outcome: super::Outcome) -> String {
    match outcome {
        super::Outcome::Rejected(error) => error.to_string(),
        super::Outcome::Failed(failure) => failure.message,
    }
}

/// How many workspaces' notebooks and folders a worker keeps between passes: those whose
/// notes moved last. One past them is read whole by its next pass.
const MOST_FOLLOWED: usize = 32;

impl Worker {
    /// Keep `followed` for `workspace_id`'s next pass, letting go of the workspace whose
    /// notes were projected longest ago once more than [`MOST_FOLLOWED`] are kept.
    fn keep_followed(&mut self, workspace_id: WorkspaceId, mut followed: Followed) {
        followed.last = tilo_clock::unix_now_ns();
        self.notes_followed.insert(workspace_id, followed);
        if self.notes_followed.len() > MOST_FOLLOWED
            && let Some(oldest) = self
                .notes_followed
                .iter()
                .min_by_key(|(_, followed)| followed.last)
                .map(|(id, _)| *id)
        {
            self.notes_followed.remove(&oldest);
        }
    }
}

/// What the host holds of a workspace's notebook and the tree its folder is, between passes
/// over them ([`Worker::project_notes`]): both as it merged them last, and which file is
/// each note's.
pub(super) struct Followed {
    notebook: AutoCommit,
    /// The notebook record's heads it merged last.
    notebook_heads: Vec<[u8; 32]>,
    tree: Namespace,
    /// Each record of the tree it holds, the tree's own (0) and its chunks, with the heads
    /// it merged last; `None` while the workspace has no tree.
    chunks: Option<HeldChunks>,
    files: Files,
    /// When its pass ended.
    last: u64,
}

impl Followed {
    /// Take the notebook and the tree as read whole: every note and every note file is to be
    /// brought into step. `None` when there is nothing to project: neither a note nor a
    /// folder.
    fn read(
        notebook: &[u8],
        notebook_heads: Vec<[u8; 32]>,
        tree: Namespace,
        chunks: Option<HeldChunks>,
    ) -> Result<Option<(Self, BTreeSet<String>)>, String> {
        let notebook = AutoCommit::load(notebook).map_err(|error| error.to_string())?;
        let pages = page_keys(&notebook);
        if chunks.is_none() && pages.is_empty() {
            return Ok(None);
        }
        let files = files_of(&tree);
        let dirty = pages.into_iter().chain(files.keys().cloned()).collect();
        Ok(Some((
            Self {
                notebook,
                notebook_heads,
                tree,
                chunks,
                files,
                last: 0,
            },
            dirty,
        )))
    }
}

#[cfg(test)]
mod tests;
