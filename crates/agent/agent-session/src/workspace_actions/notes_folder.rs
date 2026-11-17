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

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_fs::namespace::{Namespace, ROOT_INO};
use tilo_fs::tree_record::{FILES_APP, record as tree_record};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::WorkspaceId;
use ui::tilo_resource::Resource as _;

use tilo_lock::MutexRecover as _;

use super::{Needs, Worker};
use crate::participant::actions::record_provider;

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
pub(super) struct Note {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) markdown: Vec<u8>,
}

/// The notes of the notebook `doc`, each with its body as Markdown.
pub(super) fn notes(doc: &AutoCommit) -> Vec<Note> {
    let Ok(notebook) = notes::Notebook::read(doc) else {
        return Vec::new();
    };
    notebook
        .pages
        .ids()
        .filter_map(|id| {
            let page = notes::notebook::page_of(id)?;
            let note = notebook.pages.get(id)?;
            Some(Note {
                id: id.to_string(),
                title: note.title.clone(),
                markdown: body_markdown(doc, page).unwrap_or_default(),
            })
        })
        .collect()
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

/// Who wrote the newest version of the file at node `id` of the tree `doc`: the person
/// whose device signed it, and the agent or program that wrote for them. `None` when the
/// tree records no version of it.
fn writer(tree: &Namespace, doc: &AutoCommit, id: &str) -> Option<Caller> {
    let record = tree.document_id_for_id(id)?;
    let (version, attribution) = fs_protocol::history::latest(doc, record)?;
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

/// Bring the folder's note files and the notebook into step, as far as the tree alone can:
/// files written from notes that moved, files of deleted notes trashed, deleted notes'
/// files restored when the note came back, and new notes' files made. What the folder asks
/// of the notes is answered as edits to make with Notes' own actions.
pub(super) fn reconcile(
    tree: &mut Namespace,
    tree_doc: &AutoCommit,
    notes: &[Note],
    mtime: u64,
) -> Vec<NoteEdit> {
    let mut edits = Vec::new();
    let mut projected = std::collections::BTreeSet::new();
    for id in tree.projected_nodes() {
        let Some((source, agreed)) = tree.projection(&id) else {
            continue;
        };
        let Some(page) = page_of_source(&source) else {
            continue;
        };
        projected.insert(page.to_owned());
        let note = notes.iter().find(|note| note.id == page);
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
                if let Some(by) = writer(tree, tree_doc, &id) {
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
                edits.extend(follow(tree, tree_doc, &id, note, &agreed, mtime));
            }
        }
    }
    for note in notes.iter().filter(|note| !projected.contains(&note.id)) {
        make_file(tree, note, mtime);
    }
    edits
}

/// A live note file `id` and its note: written from the note when the note moved, applied
/// to the note when only the file did, and named for the note's title.
fn follow(
    tree: &mut Namespace,
    tree_doc: &AutoCommit,
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
        let Some(by) = writer(tree, tree_doc, id) else {
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

/// Make note `note`'s file in the notes folder.
fn make_file(tree: &mut Namespace, note: &Note, mtime: u64) {
    tracing::debug!(page = %note.id, "notes folder: a note's file is made");
    let Some((folder, folder_ino)) = folder(tree, mtime) else {
        return;
    };
    let name = free_name(tree, &folder, &file_name(&note.title));
    let Some(ino) = tree.create_document(folder_ino, &name, "md", mtime) else {
        return;
    };
    let Some(id) = tree.id_for_ino(ino).map(str::to_owned) else {
        return;
    };
    write_file(tree, &id, note, mtime);
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
                        crate::participant::actions::Outcome::Rejected(error) => error.to_string(),
                        crate::participant::actions::Outcome::Failed(error) => {
                            error.message.clone()
                        }
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
    pub(super) fn project_notes(&mut self, workspace_id: WorkspaceId) -> Result<(), String> {
        let tree_name = tree_record();
        let notes_route = DocumentRoute {
            app_id: NOTES_APP.to_owned(),
            cluster_id: workspace_id.to_string(),
            record: String::new(),
        };
        let tree_route = DocumentRoute {
            app_id: FILES_APP.to_owned(),
            cluster_id: workspace_id.to_string(),
            record: tree_name.clone(),
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
        // A workspace that has no notebook yet has no notes to show; one whose folder
        // nobody wrote yet gets one when the first note is made.
        let Ok((notebook, _)) = state.document(&notes_route) else {
            return Ok(());
        };
        let notebook = AutoCommit::load(&notebook).map_err(|error| error.to_string())?;
        let notes = notes(&notebook);
        let existing = state.document(&tree_route).ok().map(|(bytes, _)| bytes);
        if existing.is_none() && notes.is_empty() {
            return Ok(());
        }
        let author = hex::encode(self.host_device.key.verifying_key().to_bytes());
        let (mut tree, tree_doc) = match &existing {
            Some(bytes) => (
                Namespace::from_bytes(&author, bytes).map_err(|error| error.to_string())?,
                AutoCommit::load(bytes).map_err(|error| error.to_string())?,
            ),
            None => (
                Namespace::new(&author).map_err(|error| error.to_string())?,
                AutoCommit::new(),
            ),
        };
        let mtime = tilo_clock::unix_now_ns() / 1_000_000;
        // What loading the tree settled of its own (ids it lacked) is not this pass's
        // change: the tree is written only when the notes asked something of it.
        let loaded = tree.heads();
        let edits = reconcile(&mut tree, &tree_doc, &notes, mtime);
        tracing::debug!(
            %workspace_id,
            notes = notes.len(),
            edits = edits.len(),
            "notes folder: a pass over the notes and their files"
        );
        let applied = self.apply_note_edits(workspace_id, &notes_route, &mut tree, edits);
        if existing.is_some() && tree.heads() == loaded {
            return applied;
        }
        let mutation = if existing.is_some() {
            let change = tree.save_incremental();
            if change.is_empty() {
                return applied;
            }
            tracing::debug!(bytes = change.len(), "notes folder: the tree is written");
            DocumentMutation::Incremental(change)
        } else {
            DocumentMutation::Snapshot(tree.save())
        };
        self.commit(workspace_id, &tree_route, &mutation)?;
        applied
    }
}

#[cfg(test)]
mod tests;
