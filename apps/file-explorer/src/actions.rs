//! The file explorer's **resource tree** and the one **action** it exports.
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`](qos_actions::Action) — a serializable value that
//! deterministically changes the shared Automerge document, so undo and
//! multi-peer replay come for free — while presentation / view state (and
//! anything external) stays plain app code. Applying that split to a file
//! manager leaves almost nothing on the action side, and this module keeps that
//! finding rather than fabricate a bookmarks/tags model to have actions.
//!
//! ## What the file explorer actually is
//!
//! The file explorer is a view onto a host resource — the filesystem (the qOS
//! FUSE mount at `/var/workspace` and the user's home). Its list of entries,
//! their sizes, modified times, MIME types and per-file sync state are transient
//! filesystem data read into memory ([`crate::model::Entry`] via
//! [`read_dir_sync`](crate::model::read_dir_sync)); they are **not** collaborative
//! and never touch the document.
//!
//! Everything that changes the world — **new folder, delete-to-trash, copy,
//! move (paste), new-from-template**, and the rename the explorer finally
//! performs — is an **external** filesystem operation (see [`crate::fileops`]:
//! `std::fs` calls and the `trash` crate, which the qOS FUSE driver projects to
//! peers). It mutates the filesystem, not a shared CRDT, so by the recipe's rule
//! it is *not* an action and stays as-is. The collaborative story for a shared
//! folder is the FUSE driver replicating the bytes, not the file manager
//! encoding an edit — the recipe's "external, non-deterministic work stays out
//! of `apply`".
//!
//! ## The state that *is* persisted — and why most of it is not an action
//!
//! Two kinds of state outlive a single frame, and neither is a structural record
//! an action could mutate:
//!
//! - **Favourites / pinned places and the icon zoom level are per-user local
//!   config, not CRDT state.** They are written to plain files under
//!   `~/.config/qos-files/` (`favorites`, one path per line, and `zoom`) by
//!   [`crate::favorites::save`] / [`crate::config::save_zoom`] — never to the
//!   shared Automerge document. A pinned place is a private, machine-local
//!   shortcut, not a record two peers co-author, so it is deliberately kept off
//!   the collaborative document. There is no favourites/bookmarks/tags collection
//!   *in* the CRDT to hang add / remove / reorder actions off of, and this module
//!   does not fabricate one.
//!
//! - **The inline-rename field is native collaborative `Text`.** It is the
//!   *only* thing the app writes to its Automerge document:
//!   [`RENAME_QUERY_PATH`](crate::files_app::RENAME_QUERY_PATH)
//!   (`files.rename_query`), an Automerge `Text` object the toolkit's `TextInput`
//!   splices into on the CRDT-native text path — the same path the recipe says a
//!   flowing text body must take *instead of* the action executor. Typing into it
//!   is therefore not an action either. (There is no in-app search field — search
//!   is OS-owned, `ui/HIG.md` §5.)
//!
//! The rest of the explorer's world — the current directory, the multi-selection,
//! the sort key/order, the view mode, the navigation history, the cut/copy
//! clipboard, the show-hidden toggle — is in-memory presentation state, persisted
//! nowhere and replicated to no one.
//!
//! ## What the explorer does export
//!
//! An app whose subject matter is external is still an app someone else has to be
//! able to *see and address*. So the explorer publishes a [`Resource`] tree — the
//! open directory, and one child per entry it is listing, each addressed by its
//! real path — and exports exactly one action over the shared state it already
//! has: [`FileExplorerAction::ProposeRename`] puts a name into the rename buffer
//! for an entry.
//!
//! That tree is deliberately **not document-shaped**. There is no root document
//! and no ordered block list to walk; the ids name paths on a host filesystem the
//! CRDT knows nothing about. A participant reaching the explorer through
//! [`qos_actions::door`] enumerates a directory and names a file, and the same
//! `(resource id, action id)` address keeps meaning that file across window
//! layouts and sessions.
//!
//! Proposing is the whole of it: the buffer is shared, so the proposal is visible
//! to everyone in the room and undone like any other edit, while *performing* the
//! rename stays where it belongs — a person committing the field, and
//! [`crate::fileops::rename`] touching the disk. The directory resource itself
//! exports nothing, because new-folder and delete have no shared state to mutate.

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::files_app::RENAME_QUERY_PATH;

/// Registry `type_id` for every file-explorer action.
pub const FILE_EXPLORER_ACTION_TYPE: &str = "file-explorer.action";

/// The file explorer's one document-mutating operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FileExplorerAction {
    /// Put `name` into the shared inline-rename buffer for `path`.
    ProposeRename { path: String, name: String },
}

impl Action for FileExplorerAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let FileExplorerAction::ProposeRename { name, .. } = self;
        // `path` addresses the entry for the caller and rides the envelope so
        // the proposal is self-describing in history; the document holds only
        // the buffer, because a per-path rename record would be exactly the
        // collection this module refuses to invent.
        //
        // The buffer is seeded by `Widget::sync`. A payload that arrives before
        // the app has run a frame — or against a document that is not the
        // explorer's — names no live buffer, and is a no-op rather than an
        // error, so a bad replay cannot wedge a peer.
        let Ok(Some((Value::Object(ObjType::Text), buffer))) = doc.get(&ROOT, RENAME_QUERY_PATH)
        else {
            return Ok(());
        };
        // Diff-splice rather than rewrite the whole `Text`: it writes nothing
        // when the buffer already reads `name`, and otherwise touches only the
        // run that differs, so a proposal costs one small change instead of a
        // delete-and-reinsert of the entire value.
        ui::doc::splice_text_diffed(doc, &buffer, name);
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register the file explorer's action decoder for door invocation + remote
/// replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<FileExplorerAction>(FILE_EXPLORER_ACTION_TYPE);
}

/// Encode a [`FileExplorerAction`] into the wire form the executor dispatches.
///
/// Serializing an enum of owned `String`s cannot fail, so the error arm is
/// unreachable; it yields an empty payload rather than panicking, and the
/// registry's decode refuses that — fail-closed, the same way an unregistered
/// type is.
pub fn encode(action: &FileExplorerAction) -> EncodedAction {
    EncodedAction::encode(FILE_EXPLORER_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: FILE_EXPLORER_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

/// Propose `name` as the new name for the entry at `path`.
pub fn propose_rename_action(path: &str, name: &str) -> FileExplorerAction {
    FileExplorerAction::ProposeRename {
        path: path.to_string(),
        name: name.to_string(),
    }
}

impl ResourceProvider for crate::FileExplorerWidget {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let dir = self.current_path();
        // A directory's own name, except at a root (`/`), which has none.
        let label = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        let mut root = Resource::new(
            format!("files.directory:{}", dir.display()),
            "files.directory",
            label,
        );

        for entry in self.displayed_entries() {
            let path = entry.path.display().to_string();
            let kind = if entry.is_dir {
                "files.directory"
            } else {
                "files.entry"
            };
            // The prompted build closure outlives this frame, so it captures an
            // owned path rather than borrowing the entry.
            let action_path = path.clone();
            root = root.with_child(
                Resource::new(format!("files.entry:{path}"), kind, entry.name.clone()).with_action(
                    ActionSpec::prompted(
                        "rename",
                        "Rename",
                        "New name",
                        entry.name.clone(),
                        ExecutionLocus::ClientLocal,
                        move |text: &str| encode(&propose_rename_action(&action_path, text)),
                    ),
                ),
            );
        }

        vec![root]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ObjId;
    use automerge::transaction::Transactable;

    use qos_actions::{ActionInput, LocalTransport, door};
    use ui::widget::Widget;

    use crate::FileExplorerWidget;
    use crate::command::FileCommand;
    use crate::favorites::UserDirs;

    /// Resolve the `Text` object at `key` on the document root, asserting it is a
    /// native Automerge `Text` (the collaborative text path), not a scalar.
    fn text_obj(doc: &AutoCommit, key: &str) -> ObjId {
        match doc.get(&ROOT, key).unwrap() {
            Some((Value::Object(ObjType::Text), id)) => id,
            other => panic!("root[\"{key}\"] must be an Automerge Text, got {other:?}"),
        }
    }

    /// The document holds exactly one thing, and it is a native-`Text` input
    /// field: the inline-rename buffer. Driving the real production `sync` seeds
    /// it, and the document carries no structural, user-authored record
    /// collection — no favourites, bookmarks, tags, pins, saved searches or
    /// groupings — for an action to mutate.
    #[test]
    fn document_holds_only_ephemeral_input_text() {
        let mut w = FileExplorerWidget::new();
        let mut doc = AutoCommit::new();

        // The real first-frame path: `sync` seeds the backing Text object (the
        // toolkit's TextInput binds to an existing Text but never creates one, so
        // the app must seed it here).
        w.sync(&mut doc, &ROOT);

        // Exactly the one input field, nothing structural.
        let keys: Vec<String> = doc.keys(&ROOT).collect();
        assert_eq!(
            keys,
            vec![RENAME_QUERY_PATH.to_string()],
            "the document holds only the rename input field: {keys:?}",
        );

        // It is a native Text object (the CRDT-native collaborative text path).
        let _ = text_obj(&doc, RENAME_QUERY_PATH);

        // No user-authored record collection exists to hang an action off — the
        // favourites/tags/pins model lives in per-user config files, not here.
        // (There is no search field at all — search is OS-owned.)
        for forbidden in [
            "favorites",
            "favourites",
            "bookmarks",
            "pins",
            "places",
            "tags",
            "labels",
            "saved_searches",
            "groups",
            "files.search_query",
        ] {
            assert!(
                doc.get(&ROOT, forbidden).unwrap().is_none(),
                "document must not carry a `{forbidden}` record collection",
            );
        }
    }

    /// The rename field is native collaborative `Text`, so it already carries the
    /// guarantee an action would otherwise provide: a splice into the shared
    /// object (exactly what the `TextInput` does) replicates to an independent
    /// replica byte-identically — same head hashes, not merely the same text.
    /// Nothing to encode as an action.
    #[test]
    fn rename_field_is_native_text_and_replicates_byte_identically() {
        let mut w = FileExplorerWidget::new();
        let mut doc = AutoCommit::new();

        // Seed via the real `sync`, then snapshot a shared base state.
        w.sync(&mut doc, &ROOT);
        let rename = text_obj(&doc, RENAME_QUERY_PATH);
        doc.commit();
        let base = doc.save();
        let mut peer = AutoCommit::load(&base).unwrap();
        assert_eq!(
            doc.get_heads(),
            peer.get_heads(),
            "precondition: replicas share a base state",
        );

        // Type into the rename field the way the input does: splice into the
        // shared Text object on the native collaborative text path.
        doc.splice_text(&rename, 0, 0, "renamed.txt").unwrap();
        doc.commit();

        // The collaboration guarantee, for free: the peer converges byte-identically.
        peer.merge(&mut doc).unwrap();
        assert_eq!(
            doc.get_heads(),
            peer.get_heads(),
            "byte-identical convergence after merge — the native text path already replicates",
        );
        assert_eq!(peer.text(&rename).unwrap(), "renamed.txt", "the peer sees the same rename text");
    }

    /// A file operation is external: renaming through the real command path
    /// renames the file on disk and leaves the document's structure untouched —
    /// still only the two native-`Text` input fields, with no fabricated
    /// "renames"/"history" record. Proof that the mutation crosses the filesystem
    /// (the FUSE driver replicates it), not the action executor.
    #[test]
    fn file_operations_mutate_the_filesystem_not_the_document() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), b"x").unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(dir.path().to_path_buf());
        let mut doc = AutoCommit::new();
        w.sync(&mut doc, &ROOT);
        let keys_before: Vec<String> = {
            let mut k: Vec<String> = doc.keys(&ROOT).collect();
            k.sort();
            k
        };

        // Rename via the real command path (the command bus the UI pushes into).
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "old.txt")
            .unwrap();
        w.select(Some(idx));
        assert!(w.apply_command(FileCommand::Rename));
        w.sync(&mut doc, &ROOT); // seeds the rename Text field with the current name
        w.set_rename_buffer("new.txt".into());
        assert!(w.apply_command(FileCommand::RenameCommit));

        // The world changed on disk (external / FUSE), …
        assert!(dir.path().join("new.txt").exists(), "the file was renamed on disk");
        assert!(!dir.path().join("old.txt").exists());

        // … and the document's structure did not: no record was written for it.
        w.sync(&mut doc, &ROOT);
        let keys_after: Vec<String> = {
            let mut k: Vec<String> = doc.keys(&ROOT).collect();
            k.sort();
            k
        };
        assert_eq!(
            keys_after, keys_before,
            "a filesystem operation must not add structural keys to the CRDT: {keys_after:?}",
        );
    }

    /// Favourites (pinned places) and the icon zoom level are per-user local
    /// config, not CRDT state: the real command path writes them to plain files
    /// under the config directory and adds nothing structural to the shared
    /// document. Proof there is no in-document favourites/pins collection to hang
    /// add / remove / reorder actions off of.
    #[test]
    fn favourites_and_zoom_persist_to_disk_not_the_document() {
        let cfg = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::with_persistence_dirs(
            Some(cfg.path().to_path_buf()),
            Some(home.path().to_path_buf()),
            UserDirs::default(),
        );

        // Seed the document via the real sync, then snapshot its structure.
        let mut doc = AutoCommit::new();
        w.sync(&mut doc, &ROOT);
        let keys_before: Vec<String> = {
            let mut k: Vec<String> = doc.keys(&ROOT).collect();
            k.sort();
            k
        };

        // Pin a place and zoom in through the real command path.
        let fav = home.path().join("Projects");
        assert!(w.apply_command(FileCommand::AddFavorite(fav.clone())));
        assert!(w.apply_command(FileCommand::ZoomIn));

        // Both persisted to per-user config *files*, not the CRDT.
        assert!(cfg.path().join("favorites").exists(), "favourites persisted to a config file");
        assert!(cfg.path().join("zoom").exists(), "zoom level persisted to a config file");

        // And the shared document gained no structural record for either.
        w.sync(&mut doc, &ROOT);
        let keys_after: Vec<String> = {
            let mut k: Vec<String> = doc.keys(&ROOT).collect();
            k.sort();
            k
        };
        assert_eq!(
            keys_after, keys_before,
            "pinning a place / zooming must not add keys to the CRDT: {keys_after:?}",
        );
    }

    // The three tests below replace an earlier `exports_no_actions_and_no_resource_tree`,
    // which asserted the opposite contract: that the explorer publishes nothing at
    // all. The external-filesystem half of that argument still holds (see the tests
    // above), but a surface nobody outside the process can enumerate is not a
    // finding to preserve — an app is reachable or it is not.

    /// The production `Widget::resources` publishes the directory the explorer
    /// actually has open and the entries it is actually listing, addressed by
    /// their real paths — a resource shape that is a filesystem, not a document.
    /// The directory itself exports nothing: new-folder and delete are external
    /// work.
    #[test]
    fn publishes_the_open_directory_and_its_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(dir.path().to_path_buf());
        let mut doc = AutoCommit::new();
        w.sync(&mut doc, &ROOT);

        let resources = Widget::resources(&w, &doc);
        assert_eq!(resources.len(), 1, "one root resource: the open directory");
        let root = &resources[0];
        assert_eq!(
            root.id.as_str(),
            format!("files.directory:{}", dir.path().display()),
            "the directory is addressed by its real path",
        );
        assert_eq!(root.kind, "files.directory");
        assert_eq!(
            root.label,
            dir.path().file_name().unwrap().to_string_lossy(),
            "labelled with the directory's own name",
        );
        assert!(
            root.actions.is_empty(),
            "new folder / delete change the filesystem, not shared state — no action for them",
        );

        let entry_path = dir.path().join("notes.txt");
        let child = root
            .children
            .iter()
            .find(|c| c.id.as_str() == format!("files.entry:{}", entry_path.display()))
            .expect("the listed file is published, addressed by its real path");
        assert_eq!(child.kind, "files.entry");
        assert_eq!(child.label, "notes.txt");

        let rename = child
            .actions
            .iter()
            .find(|a| a.id == "rename")
            .expect("the entry exports `rename`");
        assert_eq!(rename.label, "Rename");
        assert_eq!(rename.locus, ExecutionLocus::ClientLocal);
        match &rename.input {
            ActionInput::Prompted {
                prompt,
                placeholder,
                ..
            } => {
                assert_eq!(prompt, "New name");
                assert_eq!(placeholder, "notes.txt", "the current name is the placeholder");
            }
            _ => panic!("rename takes a name, so it must be prompted"),
        }
    }

    /// Applying the action writes the rename buffer and *only* the rename buffer:
    /// the document is still the single native-`Text` field it was, with no
    /// per-path rename record fabricated to carry the proposal.
    #[test]
    fn proposing_a_rename_writes_only_the_rename_buffer() {
        let mut w = FileExplorerWidget::new();
        let mut doc = AutoCommit::new();
        w.sync(&mut doc, &ROOT); // the real seeding path

        let action = propose_rename_action("/var/workspace/report.txt", "quarterly.txt");
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, &action, encode(&action), &mut tx).unwrap();

        let keys: Vec<String> = doc.keys(&ROOT).collect();
        assert_eq!(
            keys,
            vec![RENAME_QUERY_PATH.to_string()],
            "the proposal added no structural record: {keys:?}",
        );
        let buffer = text_obj(&doc, RENAME_QUERY_PATH);
        assert_eq!(
            doc.text(&buffer).unwrap(),
            "quarterly.txt",
            "the shared buffer carries the proposed name",
        );
    }

    /// The point of publishing the tree: someone who is not the app can enumerate
    /// it, name one entry's rename by its stable address, perform it, and be named
    /// in history for it. Nothing here reaches into the widget — only ids, an
    /// argument, and the app's own decoder registry.
    #[test]
    fn an_addressed_rename_is_reachable_through_the_door() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("draft.txt"), b"x").unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(dir.path().to_path_buf());
        let mut doc = AutoCommit::new();
        w.sync(&mut doc, &ROOT);
        doc.commit();

        // Enumerate from outside: inert descriptors, no handle on the widget.
        let resources = Widget::resources(&w, &doc);
        let entry_path = dir.path().join("draft.txt");
        let descriptor = door::survey(&resources)
            .into_iter()
            .find(|d| {
                d.address.resource == format!("files.entry:{}", entry_path.display())
                    && d.address.action == "rename"
            })
            .expect("the entry's rename resolves to an address");
        assert!(descriptor.input.wants_text(), "the caller is told it must supply a name");
        assert_eq!(descriptor.resource_kind, "files.entry");

        let mut registry = ActionRegistry::new();
        register(&mut registry);
        let mut tx = LocalTransport::new();
        door::invoke(
            &mut doc,
            &resources,
            &descriptor.address,
            Some("final.txt"),
            b"reviewer",
            &registry,
            &mut tx,
        )
        .expect("the addressed rename performs");

        let buffer = text_obj(&doc, RENAME_QUERY_PATH);
        assert_eq!(
            doc.text(&buffer).unwrap(),
            "final.txt",
            "the caller's name landed in the shared buffer",
        );

        // Attributed to the caller, exactly like an edit someone typed.
        let changes = doc.get_changes(&[]);
        let last = changes.last().expect("the invocation committed a change");
        assert_eq!(
            last.actor_id().to_bytes(),
            b"reviewer",
            "the change commits under the caller's actor, not the app's",
        );
    }
}
