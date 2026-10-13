//! Window-local selection through the same typed door as pointer and keyboard work.

use super::{FileExplorerWidget, MAX_ANSWERED_ENTRIES};
use crate::i18n::t;
use crate::resource::MAX_MOVE_ENTRIES;
use std::collections::HashMap;

impl FileExplorerWidget {
    /// Resolve the whole request before changing anything. Paths and names are
    /// not accepted here: a caller names the rows this window publishes now.
    pub(super) fn select_published_entries(&mut self, wanted: &[String]) -> Result<(), String> {
        if wanted.len() > MAX_MOVE_ENTRIES {
            return Err(t("files-selection-too-large"));
        }
        let mut resolved: HashMap<&str, Option<usize>> =
            wanted.iter().map(|id| (id.as_str(), None)).collect();
        for (index, entry) in self.displayed_entries().iter().enumerate() {
            if let Some(found) = resolved.get_mut(Self::entry_resource_id(entry).as_str()) {
                *found = Some(index);
            }
        }
        let indices: Vec<usize> = wanted
            .iter()
            .map(|id| {
                resolved
                    .get(id.as_str())
                    .copied()
                    .flatten()
                    .ok_or_else(|| t("files-selection-unavailable"))
            })
            .collect::<Result<_, _>>()?;
        let primary = indices.first().copied();
        self.selection.replace_indices(indices, primary);
        self.retain_shown_selection();
        self.last_click = None;
        Ok(())
    }

    /// A bounded answer with stable row identities, never selected indices.
    pub(super) fn selection_answer(&self) -> serde_json::Value {
        let entries = self.displayed_entries();
        let selected = self.selection.selected();
        let ids: Vec<String> = selected
            .iter()
            .take(MAX_ANSWERED_ENTRIES)
            .filter_map(|index| entries.get(*index).map(Self::entry_resource_id))
            .collect();
        let primary = self
            .selection
            .primary()
            .and_then(|index| entries.get(index).map(Self::entry_resource_id));
        serde_json::json!({"selected": ids, "count": selected.len(), "primary": primary})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::FilesEffect;
    use ui::tilo_actions::{ActionAddress, ActionKind, door};
    use ui::tilo_resource::{Effect as _, Resource as _};

    fn invoke(
        files: &mut FileExplorerWidget,
        action: &str,
        args: &serde_json::Value,
    ) -> Result<(), String> {
        let mut doc = automerge::AutoCommit::new();
        let tree = super::super::published::tree(files, &doc);
        let encoded = door::encode_json(
            &tree,
            &ActionAddress::new(crate::resource::FileExplorer::KIND, action),
            args.as_object().unwrap(),
        )
        .map_err(|error| error.to_string())?;
        super::super::published::start(
            files,
            ui::tilo_actions::OperationKind::Effect,
            &encoded.type_id,
            &encoded.payload,
            &mut doc,
            &automerge::ROOT,
        )?;
        Ok(())
    }

    fn files(root: &std::path::Path) -> FileExplorerWidget {
        let mut files = FileExplorerWidget::with_persistence(None, Some(root.into()));
        files.navigate(root.into());
        files
    }

    #[test]
    fn published_batch_selection_copies_exact_rows_after_sorting() {
        let root = tempfile::tempdir().unwrap();
        for (name, bytes) in [("a.txt", "aa"), ("b.txt", "b"), ("c.txt", "ccc")] {
            std::fs::write(root.path().join(name), bytes).unwrap();
        }
        let dest = tempfile::tempdir().unwrap();
        let mut files = files(root.path());
        let ids: Vec<String> = ["a.txt", "c.txt"]
            .into_iter()
            .map(|name| crate::search_provider::entry_resource_id(&root.path().join(name)))
            .collect();
        invoke(
            &mut files,
            "select-entries",
            &serde_json::json!({"entries": ids}),
        )
        .unwrap();
        assert_eq!(files.selection_answer()["count"], 2);
        invoke(&mut files, "sort-size", &serde_json::json!({})).unwrap();
        invoke(&mut files, "copy-selection", &serde_json::json!({})).unwrap();
        invoke(&mut files, "clear-selection", &serde_json::json!({})).unwrap();
        assert!(files.selection.is_empty());
        assert!(invoke(&mut files, "copy-selection", &serde_json::json!({})).is_err());
        files.navigate(dest.path().into());
        invoke(&mut files, "paste", &serde_json::json!({})).unwrap();
        assert_eq!(std::fs::read(dest.path().join("a.txt")).unwrap(), b"aa");
        assert_eq!(std::fs::read(dest.path().join("c.txt")).unwrap(), b"ccc");
        assert!(!dest.path().join("b.txt").exists());
    }

    #[test]
    fn selection_request_is_atomic_bounded_and_uses_only_live_row_ids() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), b"a").unwrap();
        let mut files = files(root.path());
        invoke(&mut files, "select-all", &serde_json::json!({})).unwrap();
        let before = files.selection.clone();
        let id = crate::search_provider::entry_resource_id(&root.path().join("a.txt"));
        for entries in [
            vec![id.clone(), "missing".into()],
            vec![id.clone(); 257],
            vec!["a.txt".into()],
        ] {
            assert!(
                invoke(
                    &mut files,
                    "select-entries",
                    &serde_json::json!({"entries": entries})
                )
                .is_err()
            );
            assert_eq!(files.selection, before);
        }
        std::fs::rename(root.path().join("a.txt"), root.path().join("renamed.txt")).unwrap();
        files.refresh();
        assert!(
            invoke(
                &mut files,
                "select-entries",
                &serde_json::json!({"entries": [id]})
            )
            .is_err()
        );
        invoke(
            &mut files,
            "select-entries",
            &serde_json::json!({"entries": []}),
        )
        .unwrap();
        assert!(files.selection.is_empty());
        assert!(files.last_click.is_none());
    }

    #[test]
    fn published_selection_retains_copy_and_cut_permission_classes() {
        let specs = FilesEffect::specs();
        let permission = |id: &str| {
            specs
                .iter()
                .find(|spec| spec.id == id)
                .unwrap()
                .contract
                .kind
        };
        assert_eq!(permission("copy-selection"), ActionKind::Read);
        assert_eq!(permission("cut-selection"), ActionKind::Outside);
        for id in ["select-entries", "select-all", "clear-selection"] {
            assert_eq!(permission(id), ActionKind::Read);
            assert_eq!(
                specs.iter().find(|spec| spec.id == id).unwrap().kind,
                ui::tilo_actions::OperationKind::View
            );
        }
    }

    #[test]
    fn published_selection_retains_original_object_when_its_name_is_reused() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.txt");
        std::fs::write(&path, b"shown").unwrap();
        let mut files = files(root.path());
        invoke(&mut files, "select-all", &serde_json::json!({})).unwrap();
        let retained = files.shown_selection.objects[&path].1.clone().unwrap();
        let before = retained.observed();
        std::fs::rename(&path, root.path().join("original.txt")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        // Selecting the same published row again cannot silently reopen its
        // pathname. The selection and retained descriptor still name "shown".
        let id = FileExplorerWidget::entry_resource_id(&files.entries[0]);
        invoke(
            &mut files,
            "select-entries",
            &serde_json::json!({"entries": [id]}),
        )
        .unwrap();
        let current = files.shown_selection.objects[&path].1.as_ref().unwrap();
        assert!(std::sync::Arc::ptr_eq(&retained, current));
        assert!(current.retains(before));
        let replacement =
            host_fs::ObservedObject::from_metadata(&std::fs::symlink_metadata(&path).unwrap())
                .unwrap();
        assert!(!current.retains(replacement));
        invoke(&mut files, "clear-selection", &serde_json::json!({})).unwrap();
        assert!(files.shown_selection.objects.is_empty());
    }

    #[test]
    fn published_selection_remembers_a_replaced_row_refusal_until_deselected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.txt");
        std::fs::write(&path, b"shown").unwrap();
        let mut files = files(root.path());
        std::fs::rename(&path, root.path().join("original.txt")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        invoke(&mut files, "select-all", &serde_json::json!({})).unwrap();
        assert!(files.shown_selection.objects[&path].1.is_none());
        invoke(&mut files, "select-all", &serde_json::json!({})).unwrap();
        assert!(files.shown_selection.objects[&path].1.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    }

    #[test]
    fn background_listing_refresh_cannot_adopt_a_selected_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.txt");
        std::fs::write(&path, b"shown").unwrap();
        let mut files = files(root.path());
        invoke(&mut files, "select-all", &serde_json::json!({})).unwrap();
        std::fs::rename(&path, root.path().join("original.txt")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(files.refresh_metadata_preserving_view());
        assert_eq!(files.selection.selected().len(), 1);
        assert!(files.shown_selection.objects[&path].1.is_none());
        // An explicit deselection and selection of the new listing may retain
        // the replacement: this is a new user selection, not a passive update.
        invoke(&mut files, "clear-selection", &serde_json::json!({})).unwrap();
        let entry = files
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap();
        let id = FileExplorerWidget::entry_resource_id(entry);
        invoke(
            &mut files,
            "select-entries",
            &serde_json::json!({"entries": [id]}),
        )
        .unwrap();
        let observed = files
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap()
            .observed
            .unwrap();
        assert!(
            files.shown_selection.objects[&path]
                .1
                .as_ref()
                .unwrap()
                .retains(observed)
        );
    }

    #[test]
    fn selection_descriptor_budget_is_bounded_even_for_select_all() {
        // The full budget is one descriptor per entry; an app launch raises
        // the open-file limit to fit it (macOS starts processes at 256).
        tilo_process::raise_open_file_limit();
        let root = tempfile::tempdir().unwrap();
        for index in 0..=MAX_MOVE_ENTRIES {
            std::fs::write(root.path().join(format!("{index}.txt")), b"shown").unwrap();
        }
        let mut files = files(root.path());
        invoke(&mut files, "select-all", &serde_json::json!({})).unwrap();
        assert_eq!(files.selection.selected().len(), MAX_MOVE_ENTRIES + 1);
        assert!(files.shown_selection.objects.is_empty());
        let ids = files
            .entries
            .iter()
            .take(MAX_MOVE_ENTRIES)
            .map(FileExplorerWidget::entry_resource_id)
            .collect::<Vec<_>>();
        invoke(
            &mut files,
            "select-entries",
            &serde_json::json!({"entries": ids}),
        )
        .unwrap();
        assert_eq!(files.shown_selection.objects.len(), MAX_MOVE_ENTRIES);
        assert!(
            files
                .shown_selection
                .objects
                .values()
                .all(|(_, retained)| retained.is_some())
        );
        invoke(&mut files, "clear-selection", &serde_json::json!({})).unwrap();
        assert!(files.shown_selection.objects.is_empty());
    }

    #[test]
    fn navigating_retains_the_shown_folder_and_virtual_views_have_no_destination() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("destination");
        std::fs::create_dir(&path).unwrap();
        let mut files = files(&path);
        let retained = files.shown_folder.clone().unwrap();
        std::fs::rename(&path, root.path().join("original")).unwrap();
        std::fs::create_dir(&path).unwrap();
        let replacement =
            host_fs::ObservedObject::from_metadata(&std::fs::symlink_metadata(&path).unwrap())
                .unwrap();
        assert!(!retained.retains(replacement));
        assert!(std::sync::Arc::ptr_eq(
            &retained,
            files.shown_folder.as_ref().unwrap()
        ));
        files.navigate(crate::recent::location());
        assert!(files.shown_folder.is_none());
    }

    #[test]
    fn pointer_selection_and_context_menu_keep_the_same_retained_multi_selection() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.path().join(name), name).unwrap();
        }
        let mut files = files(root.path());
        assert!(files.apply_command(crate::command::FileCommand::Click {
            index: 0,
            ctrl: false,
            shift: false,
        }));
        assert!(files.apply_command(crate::command::FileCommand::Click {
            index: 2,
            ctrl: true,
            shift: false,
        }));
        let path = root.path().join("a.txt");
        let retained = files.shown_selection.objects[&path].1.clone().unwrap();
        std::fs::rename(&path, root.path().join("original.txt")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(files.apply_command(crate::command::FileCommand::ContextSelect(Some(0))));
        assert_eq!(files.selection.selected(), vec![0, 2]);
        assert!(std::sync::Arc::ptr_eq(
            &retained,
            files.shown_selection.objects[&path].1.as_ref().unwrap()
        ));
        assert!(files.apply_command(crate::command::FileCommand::ContextSelect(None)));
        assert!(files.shown_selection.objects.is_empty());
    }
}
