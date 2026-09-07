//! Resource + action-model status for the file explorer.
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`](qos_actions::Action) — a serializable value that
//! deterministically changes the shared Automerge document, so undo and
//! multi-peer replay come for free — while presentation / view state (and
//! anything external) stays plain app code. Applying that split to the file
//! explorer leaves **nothing** on the action side, and this module records that
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
//! Everything that changes the world — **new folder, rename, delete-to-trash,
//! copy, move (paste), new-from-template** — is an **external** filesystem
//! operation (see [`crate::fileops`]: `std::fs` calls and the `trash` crate,
//! which the qOS FUSE driver projects to peers). It mutates the filesystem, not a
//! shared CRDT, so by the recipe's rule it is *not* an action and stays as-is.
//! The collaborative story for a shared folder is the FUSE driver replicating the
//! bytes, not the file manager encoding an edit — the recipe's "external,
//! non-deterministic work stays out of `apply`".
//!
//! ## The state that *is* persisted — and why none of it is an action
//!
//! Two kinds of state outlive a single frame, and neither is a document mutation:
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
//!   [`RENAME_QUERY_PATH`](crate::files_app) (`files.rename_query`), an Automerge
//!   `Text` object the toolkit's `TextInput` splices into on the CRDT-native text
//!   path — the same path the recipe says a flowing text body must take *instead
//!   of* the action executor. It is transient presentation input: the rename
//!   field's committed value is handed straight to the external `fileops::rename`,
//!   then discarded. Re-encoding a keystroke into it as an action would only
//!   duplicate what the `Text` object already replicates and undoes. (There is no
//!   in-app search field — search is OS-owned, `ui/HIG.md` §5.)
//!
//! The rest of the explorer's world — the current directory, the multi-selection,
//! the sort key/order, the view mode, the navigation history, the cut/copy
//! clipboard, the show-hidden toggle — is in-memory presentation state, persisted
//! nowhere and replicated to no one.
//!
//! ## Consequence
//!
//! The file explorer exports no replayable CRDT actions, but it does publish a
//! Ctrl+K resource tree of local filesystem and View operations. Search hits,
//! visible controls, accessibility activation and headless agents therefore
//! converge on exported `open` / `reveal` addresses without pretending that
//! navigation mutates the shared document. `take_committed_action` remains
//! `None` because these operations create no CRDT undo boundary. The tests
//! below prove each half of this split on the real production path: the
//! document holds only the two native-`Text` input fields (no structural
//! record), the search field replicates byte-identically for free, file
//! operations mutate the filesystem while leaving the document's structure
//! untouched, and favourites + zoom persist to per-user config files rather
//! than the CRDT.

#[cfg(test)]
mod tests {
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

    use ui::widget::Widget;

    use crate::FileExplorerWidget;
    use crate::command::FileCommand;
    use crate::favorites::UserDirs;

    /// The CRDT map key the app seeds (kept in sync with `files_app.rs`).
    const RENAME_QUERY_PATH: &str = "files.rename_query";

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
        assert_eq!(
            peer.text(&rename).unwrap(),
            "renamed.txt",
            "the peer sees the same rename text"
        );
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
        assert!(
            dir.path().join("new.txt").exists(),
            "the file was renamed on disk"
        );
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
        assert!(w.apply_command(FileCommand::AddFavorite(fav)));
        assert!(w.apply_command(FileCommand::ZoomIn));

        // Both persisted to per-user config *files*, not the CRDT.
        assert!(
            cfg.path().join("favorites").exists(),
            "favourites persisted to a config file"
        );
        assert!(
            cfg.path().join("zoom").exists(),
            "zoom level persisted to a config file"
        );

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

    /// Filesystem operations are exported as local resource operations, not
    /// replayable CRDT actions. Publishing them must not fabricate an action
    /// host undo boundary.
    #[test]
    fn exports_local_resources_without_crdt_actions() {
        let mut w = FileExplorerWidget::new();
        let mut doc = AutoCommit::new();
        w.sync(&mut doc, &ROOT);

        let resources = Widget::resources(&w, &doc);
        assert_eq!(resources.len(), 1, "one current-folder root");
        let folder = &resources[0];
        assert_eq!(folder.kind, "files.folder");
        let actions: Vec<&str> = folder
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        // Typing an arbitrary local path remains a private, in-window path-bar
        // interaction. It is deliberately not published as a resource action:
        // an action address can outlive this process, whereas a host path must
        // never be replicated or retained by search/history agents.
        for expected in ["refresh", "new-folder", "paste"] {
            assert!(
                actions.contains(&expected),
                "folder publishes {expected}: {actions:?}",
            );
        }
        assert!(
            !actions.contains(&"navigate"),
            "private path-bar navigation is not a durable resource action: {actions:?}"
        );
        assert!(
            w.take_committed_action().is_none(),
            "no action is dispatched, so no action-host undo boundary is recorded",
        );
    }
}
