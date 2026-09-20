use super::*;

#[cfg(test)]
mod open_with_tests {
    use super::*;
    use crate::model::Entry;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct RecRunner {
        calls: Vec<(String, Option<PathBuf>)>,
        /// Paths handed to the system default handler (non-document opens).
        opened: Vec<PathBuf>,
    }
    impl CommandRunner for RecRunner {
        fn spawn(&mut self, program: &str, _cwd: &Path) -> std::io::Result<()> {
            self.calls.push((program.to_string(), None));
            Ok(())
        }
        fn spawn_arg(&mut self, program: &str, arg: &Path, _cwd: &Path) -> std::io::Result<()> {
            self.calls
                .push((program.to_string(), Some(arg.to_path_buf())));
            Ok(())
        }
        fn open_default(&mut self, path: &Path) -> std::io::Result<()> {
            self.opened.push(path.to_path_buf());
            Ok(())
        }
    }

    fn file_entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/docs").join(name),
            is_dir: false,
            size: 0,
            modified: None,
            mime: String::new(),
            sync_state: None,
            content_hash: None,
            record_id: None,
            availability: None,
            share: None,
            history: None,
            rich_metadata: None,
        }
    }

    #[test]
    fn document_extensions_are_recognised_as_documents() {
        for f in ["report.odt", "memo.docx", "notes.md", "readme.markdown"] {
            assert!(is_document(Path::new(f)), "{f}");
        }
        assert!(!is_document(Path::new("photo.png")));
        assert!(!is_document(Path::new("main.rs")));
    }

    /// Every activated file goes to the host, whatever its type. The
    /// explorer must not sort files into "documents" and "everything
    /// else" — which app owns a `.pdf` or a `.png` is the desktop's
    /// answer, from what is installed, and a table here would be wrong
    /// the moment anybody installs anything.
    #[test]
    fn activating_a_file_hands_the_path_to_the_host() {
        let asked: std::rc::Rc<std::cell::RefCell<Vec<PathBuf>>> = std::rc::Rc::default();
        let sink = asked.clone();
        set_document_opener(move |p| {
            sink.borrow_mut().push(p.to_path_buf());
            Ok(())
        });

        let mut w = FileExplorerWidget::new();
        w.entries = ["report.odt", "photo.png", "slides.pdf", "main.rs"]
            .iter()
            .map(|f| file_entry(f))
            .collect();
        let mut runner = RecRunner::default();
        for i in 0..4 {
            assert!(w.open_entry(i, &mut runner).unwrap(), "entry {i} opens");
        }

        assert_eq!(
            *asked.borrow(),
            vec![
                PathBuf::from("/docs/report.odt"),
                PathBuf::from("/docs/photo.png"),
                PathBuf::from("/docs/slides.pdf"),
                PathBuf::from("/docs/main.rs"),
            ],
            "the host is asked for every file, with its own path"
        );
        assert!(
            runner.calls.is_empty(),
            "the explorer launches nothing itself: {:?}",
            runner.calls
        );
        assert!(
            runner.opened.is_empty(),
            "and does not go behind the host's back to xdg-open"
        );
        assert_eq!(w.status, None, "a successful open reports nothing");
        clear_document_opener();
    }

    /// A host that cannot open the file says why, and the explorer shows
    /// it. Activation is never silent.
    #[test]
    fn a_refused_open_is_shown_to_the_user() {
        set_document_opener(|_| Err("no installed app opens .zzz files (thing.zzz)".into()));
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("thing.zzz")];
        let mut runner = RecRunner::default();
        assert!(w.open_entry(0, &mut runner).unwrap());
        let status = w
            .status
            .clone()
            .expect("the refusal reaches the status bar");
        assert!(status.contains("thing.zzz"), "must name the file: {status}");
        assert!(runner.opened.is_empty(), "a refusal is not a fallback");
        clear_document_opener();
    }

    /// With no host installed — the explorer run outside a Tilo session —
    /// activation still opens the file, via the desktop's own handler,
    /// rather than doing nothing.
    #[test]
    fn without_a_host_every_type_falls_back_to_the_desktop_handler() {
        clear_document_opener();
        for f in [
            "pic.jpg",
            "movie.mp4",
            "song.flac",
            "doc.pdf",
            "main.rs",
            "report.odt",
        ] {
            let mut w = FileExplorerWidget::new();
            w.entries = vec![file_entry(f)];
            let mut runner = RecRunner::default();
            assert!(w.open_entry(0, &mut runner).unwrap(), "{f} should open");
            assert!(runner.calls.is_empty(), "{f} must not spawn a bare binary");
            assert_eq!(runner.opened, vec![PathBuf::from("/docs").join(f)], "{f}");
        }
    }

    #[test]
    fn context_menu_opens_with_selection_aware_items() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt"), file_entry("b.txt")];
        w.selection.select_only(0);
        w.open_context_menu(Point::new(40.0, 40.0));
        assert!(w.context_menu.is_open());
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(std::string::ToString::to_string))
            .collect();
        // Selection-aware actions are present.
        for expected in [
            "Open",
            "Rename",
            "Cut",
            "Copy",
            "Move to Trash",
            "New Folder",
        ] {
            assert!(
                labels.contains(&expected.to_string()),
                "menu missing {expected}: {labels:?}"
            );
        }
        // No clipboard yet → no Paste.
        assert!(!labels.contains(&"Paste".to_string()));
    }

    #[test]
    fn empty_selection_menu_omits_item_actions() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt")];
        // No selection.
        w.open_context_menu(Point::new(10.0, 10.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(std::string::ToString::to_string))
            .collect();
        assert!(!labels.contains(&"Move to Trash".to_string()));
        assert!(labels.contains(&"New Folder".to_string()));
        assert!(labels.contains(&"Open Terminal Here".to_string()));
    }

    #[test]
    fn copy_then_paste_menu_includes_paste() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt")];
        w.selection.select_only(0);
        // Simulate a copy populating the clipboard.
        w.apply_command(crate::command::FileCommand::Copy);
        assert_eq!(w.clipboard.len(), 1);
        w.open_context_menu(Point::new(10.0, 10.0));
        assert!(
            w.context_menu
                .items()
                .iter()
                .filter_map(|i| i.label().map(std::string::ToString::to_string))
                .any(|label| label == "Paste")
        );
    }
}

/// Tests for the real command-application paths — the things a user actually
/// triggers from the toolbar / context menu / keyboard, exercised through
/// `apply_command`, `sync`, and the fs mutations (never through a tautological
/// helper). These are the paths the review flagged as untested.
#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod command_path_tests {
    use super::*;
    use crate::model::Entry;
    use std::path::PathBuf;

    #[test]
    fn opaque_hold_deadline_reaches_files_root_before_metadata_refresh() {
        let mut scroll = ui::widget::Scrollable::new_vertical();
        scroll.set_viewport_size(200.0, 300.0);
        scroll.set_content_size(200.0, 1200.0);
        scroll.scroll_by(0.0, 20.0);
        let deadline = scroll
            .next_wake_deadline()
            .expect("opaque hold schedules fade start");
        let mut widget = FileExplorerWidget::new();
        widget.nav.set_content(Box::new(scroll));
        widget.next_metadata_refresh = deadline + std::time::Duration::from_secs(60);
        assert_eq!(widget.next_wake_deadline(), Some(deadline));
        widget.next_metadata_refresh = deadline - std::time::Duration::from_millis(1);
        assert_eq!(
            widget.next_wake_deadline(),
            Some(widget.next_metadata_refresh)
        );
    }

    /// In-memory entry with a controllable name/mime (no fs touch).
    fn ent(name: &str, is_dir: bool, mime: &str) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from(name),
            is_dir,
            size: 0,
            modified: None,
            mime: mime.into(),
            sync_state: None,
            content_hash: None,
            record_id: None,
            availability: None,
            share: None,
            history: None,
            rich_metadata: None,
        }
    }

    #[test]
    fn set_sort_command_selects_the_key_including_type() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![
            ent("b.png", false, "image/png"),
            ent("a.txt", false, "text/plain"),
        ];
        // SortBy::Type is ONLY reachable via this command (no column/button),
        // so exercising it proves the previously-unreachable key now works.
        assert!(w.apply_command(FileCommand::SetSort(SortBy::Type)));
        assert_eq!(w.sort_by(), SortBy::Type);
        assert_eq!(
            w.entries_snapshot()[0].name,
            "b.png",
            "image/png < text/plain"
        );

        assert!(w.apply_command(FileCommand::SetSort(SortBy::Name)));
        assert_eq!(w.sort_by(), SortBy::Name);
        assert_eq!(w.entries_snapshot()[0].name, "a.txt");
    }

    #[test]
    fn command_l_opens_the_editable_location_bar_without_a_pointer() {
        use winit::keyboard::{Key, KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey};

        let mut widget = FileExplorerWidget::new();
        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Character(winit::keyboard::SmolStr::new("l"));
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = winit::event::ElementState::Pressed;
        event.repeat = false;

        assert!(widget.handle_app_key(
            &event,
            winit::event::Modifiers::from(ModifiersState::CONTROL),
        ));
        assert!(
            widget.toolbar.is_path_editing(),
            "the documented location shortcut must expose the real editable path control"
        );
    }

    #[test]
    fn shortcut_reference_is_a_keyboard_and_command_reachable_view() {
        let mut widget = FileExplorerWidget::new();
        use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};

        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Named(NamedKey::F1);
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = winit::event::ElementState::Pressed;
        event.repeat = false;
        assert!(widget.handle_app_key(&event, winit::event::Modifiers::default()));
        let panel = widget
            .collab_panel
            .as_ref()
            .expect("shortcut reference opens");
        assert_eq!(panel.title, "Keyboard shortcuts");
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("Grid navigation"))
        );
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("Primary modifier"))
        );
    }

    #[test]
    #[cfg(unix)]
    fn trash_view_menu_offers_restore_and_empty_not_move_to_trash() {
        let Some(trash) = crate::fileops::trash_dir() else {
            return;
        };
        let mut w = FileExplorerWidget::new();
        // Pretend we are viewing the Trash (without touching the real trash).
        w.current_path = trash;
        w.entries = vec![ent("deleted.txt", false, "text/plain")];
        w.selection.select_only(0);
        w.open_context_menu(Point::new(5.0, 5.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect();
        assert!(
            labels.contains(&"Restore".to_string()),
            "trash menu offers Restore: {labels:?}"
        );
        assert!(
            labels.contains(&"Empty Trash".to_string()),
            "and Empty Trash: {labels:?}"
        );
        assert!(
            labels.contains(&"Delete Permanently".to_string()),
            "and a permanent delete"
        );
        assert!(
            !labels.contains(&"Move to Trash".to_string()),
            "not re-trash (already trashed)"
        );
        assert!(
            !labels.contains(&"Rename".to_string()),
            "no in-place editing in the trash"
        );
    }

    #[test]
    fn resource_catalogue_exposes_view_and_favourite_commands_through_the_action_door() {
        let state_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let folder = home_dir.path().join("agent-folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("b.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::with_persistence(
            Some(state_dir.path().to_path_buf()),
            Some(home_dir.path().to_path_buf()),
        );
        w.navigate(folder.clone());
        let initial_zoom = w.zoom_level();
        let doc = automerge::AutoCommit::new();
        let resources = Widget::resources(&w, &doc);
        let folder_resource = &resources[0];
        for action in [
            "grid",
            "list",
            "sort-name",
            "sort-size",
            "sort-type",
            "sort-modified",
            "sort-ascending",
            "sort-descending",
            "zoom-in",
            "zoom-out",
            "keyboard-shortcuts",
            "add-favorite",
        ] {
            assert!(
                folder_resource.actions.iter().any(|spec| spec.id == action),
                "folder resource must export {action}"
            );
        }
        assert!(Widget::local_resource_operation_handlers(&w).contains(&"files.remote-dav"));

        for command in ["grid", "sort-size", "zoom-in", "shortcuts", "add-favorite"] {
            assert!(
                Widget::invoke_resource_operation(
                    &mut w,
                    if command == "grid"
                        || command == "sort-size"
                        || command == "zoom-in"
                        || command == "shortcuts"
                    {
                        OperationKind::View
                    } else {
                        OperationKind::Effect
                    },
                    "files.folder-command",
                    &serde_json::to_vec(command).unwrap(),
                    &doc,
                    &automerge::ROOT,
                )
                .unwrap(),
                "{command} is routed through the canonical dispatcher"
            );
        }
        assert_eq!(w.view_mode(), ViewMode::Grid);
        assert_eq!(w.sort_by(), SortBy::Size);
        assert!(w.zoom_level() > initial_zoom);
        assert_eq!(
            w.collab_panel.as_ref().map(|panel| panel.title.as_str()),
            Some("Keyboard shortcuts"),
            "the resource action opens the same reference as the menu and keyboard"
        );
        assert!(w.favorites().iter().any(|favorite| favorite.path == folder));
    }

    #[test]
    fn locally_verified_trash_undo_is_a_canonical_nonreplayable_folder_action() {
        let state_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::with_persistence(
            Some(state_dir.path().to_path_buf()),
            Some(home_dir.path().to_path_buf()),
        );
        // This is intentionally a synthetic, non-existent Trash path. The
        // resource contract is testable without touching a developer's real
        // Trash; runtime revalidation is covered by the command itself.
        w.last_trash_undo = Some(vec![PathBuf::from("/not-a-real-trash-entry")]);
        let doc = automerge::AutoCommit::new();
        let folder = Widget::resources(&w, &doc)
            .into_iter()
            .next()
            .expect("current folder resource");
        let undo = folder
            .actions
            .iter()
            .find(|action| action.id == "undo-trash")
            .expect("verified local undo is exported through the folder resource");
        assert_eq!(undo.kind, OperationKind::Effect);
        assert_eq!(undo.operation_type.as_deref(), Some("files.folder-command"));
        assert_eq!(undo.contract.permission, PermissionRequirement::Write);
        assert_eq!(undo.contract.retry, RetryPolicy::AtMostOnce);
        assert!(
            !undo.contract.reversible,
            "filesystem restore is not replayable history"
        );

        assert!(
            Widget::invoke_resource_operation(
                &mut w,
                OperationKind::Effect,
                "files.folder-command",
                &serde_json::to_vec("undo-trash").unwrap(),
                &doc,
                &automerge::ROOT,
            )
            .unwrap()
        );
        assert!(
            w.last_trash_undo.is_none(),
            "a stale external Trash state clears the ephemeral local affordance"
        );
    }

    #[test]
    fn large_text_view_menu_clamps_undo_inside_the_live_window() {
        let mut w = FileExplorerWidget::new();
        w.rect = vello::kurbo::Rect::new(0.0, 0.0, 1440.0, 900.0);
        let mut menu = w.new_overlay_menu();
        menu.push(MenuItem::action(t("files-undo-move-trash"), || {}));
        // Model a top-right toolbar trigger before the menu paints at 200%
        // text. The overlay must use the window as its viewport, not the
        // normal-scale trigger's original right-edge position.
        menu.open_at([1428.0, 52.0]);
        menu.set_text_scale(2.0);
        let menu = menu.get_rect();
        assert!(menu.x0 >= w.rect.x0 && menu.x1 <= w.rect.x1, "{menu:?}");
        assert!(menu.y0 >= w.rect.y0 && menu.y1 <= w.rect.y1, "{menu:?}");
        assert!(
            menu.width() >= 400.0,
            "the scaled menu keeps enough width for the Undo label: {menu:?}"
        );
    }

    #[test]
    fn remote_dav_allows_recovery_from_stale_offline_but_honours_disabled() {
        assert_eq!(
            remote_dav_availability(crate::remote_accounts::RemoteAccountHealth::Offline),
            Availability::Available,
            "Offline is last observed status, so the next DAV attempt may discover recovery"
        );
        assert!(matches!(
            remote_dav_availability(crate::remote_accounts::RemoteAccountHealth::Disabled),
            Availability::Unavailable { .. }
        ));
    }

    #[test]
    fn template_resource_ids_are_stable_bounded_and_do_not_disclose_paths() {
        let first = "/home/alice/Templates/Plan.md";
        let second = "/home/alice/Templates/Notes.md";
        let first_id = template_resource_id(first);
        assert_eq!(first_id, template_resource_id(first));
        assert_ne!(first_id, template_resource_id(second));
        assert!(first_id.starts_with("files.template:"));
        assert_eq!(first_id.len(), "files.template:".len() + 32);
        assert!(!first_id.contains("alice"));
        assert!(!first_id.contains("Templates"));
    }

    #[test]
    fn projected_availability_controls_are_discoverable_and_use_the_entry_route() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("shared.txt");
        std::fs::write(&path, b"x").unwrap();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = path;
        entry.availability = Some(crate::model::AvailabilityProjection {
            schema_version: 1,
            record_id: "record".into(),
            current_version: Some("v1".into()),
            residency: crate::model::Residency::Partial { resident_bytes: 1 },
            transfer: crate::model::ProjectedTransfer {
                phase: crate::model::TransferPhase::Idle,
                progress: None,
                last_error: None,
                last_error_at_ms: None,
                error_truncated: false,
            },
            policy: crate::model::AvailabilityPolicy::Automatic,
            policy_inherited: false,
            replicas: crate::model::ReplicaEvidence::Unknown,
            freshness: crate::model::AvailabilityFreshness::default(),
        });
        let mut w = FileExplorerWidget::new();
        w.current_path = d.path().to_path_buf();
        w.entries = vec![entry];
        let doc = automerge::AutoCommit::new();
        let resources = Widget::resources(&w, &doc);
        let resource = resources[0]
            .children
            .iter()
            .find(|resource| resource.label == "shared.txt")
            .expect("the projected entry is a resource");
        for action in [
            "availability",
            "keep-offline",
            "free-local-space",
            "retry-download",
        ] {
            assert!(
                resource.actions.iter().any(|spec| spec.id == action),
                "availability projection must expose {action}"
            );
        }
        let payload = vec![
            "availability".to_string(),
            FileExplorerWidget::entry_resource_id(&w.entries[0]),
        ];
        assert!(
            Widget::invoke_resource_operation(
                &mut w,
                OperationKind::View,
                "files.entry-command",
                &serde_json::to_vec(&payload).unwrap(),
                &doc,
                &automerge::ROOT,
            )
            .unwrap()
        );
        assert!(w.collab_panel.is_some(), "availability route opens details");
    }

    #[test]
    fn trash_actions_require_an_explicit_irrevocable_confirmation() {
        let Some(trash) = crate::fileops::trash_dir() else {
            return;
        };
        let mut w = FileExplorerWidget::new();
        // Keep this entirely in-memory: the assertion is the interaction
        // boundary, before Files ever calls the OS trash implementation.
        w.current_path = trash;
        w.entries = vec![ent("deleted.txt", false, "text/plain")];
        w.selection.select_only(0);

        assert!(w.apply_command(FileCommand::Delete));
        let panel = w.collab_panel.as_ref().expect("permanent delete warning");
        assert!(matches!(
            panel.action.as_ref(),
            Some(CollabPanelAction::PermanentDelete(_))
        ));
        assert_eq!(
            w.collab_panel_action_command(),
            Some(FileCommand::ConfirmPermanentDelete {
                paths: vec![PathBuf::from("deleted.txt")],
            }),
            "only the named warning action may invoke the purge"
        );
        let dialog = Widget::accessibility(&w).expect("warning has semantics");
        assert_eq!(dialog.role, ui::a11y::Role::Dialog);
        assert_eq!(
            dialog.children.last().and_then(|node| node.name.as_deref()),
            Some("Delete Permanently")
        );

        w.close_collab_panel();
        assert!(w.apply_command(FileCommand::EmptyTrash));
        assert!(matches!(
            w.collab_panel
                .as_ref()
                .and_then(|panel| panel.action.as_ref()),
            Some(CollabPanelAction::EmptyTrash)
        ));
        assert_eq!(
            w.collab_panel_action_command(),
            Some(FileCommand::ConfirmEmptyTrash),
            "emptying Trash must use a separate explicit action"
        );
    }

    #[test]
    fn context_menu_exposes_the_sort_section_including_type() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![ent("a.txt", false, "text/plain")];
        w.open_context_menu(Point::new(5.0, 5.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect();
        for s in [
            "Sort by Name",
            "Sort by Size",
            "Sort by Type",
            "Sort by Date",
        ] {
            assert!(
                labels.contains(&s.to_string()),
                "menu missing {s}: {labels:?}"
            );
        }
    }

    #[test]
    fn state_surface_copy_is_bounded_and_keeps_the_folder_context() {
        let mut w = FileExplorerWidget::new();
        w.current_path = PathBuf::from(format!(
            "/workspace/{}",
            "a-very-long-folder-name/".repeat(8)
        ));
        w.error = Some("permission denied while opening this folder".repeat(8));

        let (title, detail, can_retry) = w.state_surface_copy().expect("error state copy");
        assert_eq!(title, t("files-load-error-title"));
        assert!(can_retry);
        assert!(
            detail.contains("…"),
            "long location/error is bounded: {detail}"
        );
        assert!(
            detail.chars().count() < 190,
            "recovery copy stays readable on narrow screens: {detail}"
        );
    }

    #[test]
    fn retry_refresh_recovers_from_error_once_the_dir_exists() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("appears-later");
        let mut w = FileExplorerWidget::new();
        w.navigate(target.clone());
        assert!(w.error_message().is_some(), "missing dir sets an error");
        // The "Try Again" button pushes Refresh — still errors while missing.
        assert!(w.apply_command(FileCommand::Refresh));
        assert!(w.error_message().is_some());
        // Once the directory exists, the same retry clears the error.
        std::fs::create_dir(&target).unwrap();
        assert!(w.apply_command(FileCommand::Refresh));
        assert!(
            w.error_message().is_none(),
            "retry succeeds once the dir exists"
        );
    }

    #[test]
    fn rename_flow_renames_the_selected_file_on_disk() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("old.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "old.txt")
            .unwrap();
        w.select(Some(idx));

        // Begin the rename via the real command; the bar seeds the name.
        assert!(w.apply_command(FileCommand::Rename));
        assert!(w.is_renaming());
        assert_eq!(w.rename_buffer(), "old.txt");

        // Type a new name + commit through the command path.
        w.set_rename_buffer("new.txt".into());
        assert!(w.apply_command(FileCommand::RenameCommit));
        assert!(!w.is_renaming(), "commit closes the rename bar");
        assert!(d.path().join("new.txt").exists(), "file renamed on disk");
        assert!(!d.path().join("old.txt").exists());
    }

    #[test]
    fn reseeding_the_rename_field_never_mutates_the_shared_document() {
        // An in-progress filename is device-local interaction state. Repeated
        // renames must not create an ever-growing CRDT history, leak a path to
        // collaborators, or cause a remote client to see a half-typed name.
        let d = tempfile::tempdir().unwrap();
        for i in 0..40 {
            std::fs::write(d.path().join(format!("report-{i:03}.txt")), b"x").unwrap();
        }
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());

        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let baseline = doc.save();
        w.set_rename_buffer("report-000.txt".into());
        w.sync(&mut doc, &root);

        for i in 1..40 {
            w.set_rename_buffer(format!("report-{i:03}.txt"));
            w.sync(&mut doc, &root);
        }
        assert_eq!(
            w.rename_buffer(),
            "report-039.txt",
            "the local editor holds the last seeded name"
        );
        assert_eq!(
            doc.save(),
            baseline,
            "rename UI state must not be written into the shared document"
        );
    }

    #[test]
    fn rename_commit_is_a_noop_for_blank_or_unchanged_names() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("keep.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "keep.txt")
            .unwrap();

        w.select(Some(idx));
        w.begin_rename();
        w.set_rename_buffer("   ".into());
        assert!(!w.commit_rename(), "blank name does not rename");
        assert!(!w.is_renaming());
        assert!(d.path().join("keep.txt").exists());

        w.select(Some(idx));
        w.begin_rename();
        w.set_rename_buffer("keep.txt".into());
        assert!(!w.commit_rename(), "unchanged name does not rename");
        assert!(d.path().join("keep.txt").exists());
    }

    #[test]
    fn rename_cancel_leaves_the_file_untouched() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("stay.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "stay.txt")
            .unwrap();
        w.select(Some(idx));
        w.apply_command(FileCommand::Rename);
        w.set_rename_buffer("other.txt".into());
        assert!(w.apply_command(FileCommand::RenameCancel));
        assert!(!w.is_renaming());
        assert!(d.path().join("stay.txt").exists());
        assert!(!d.path().join("other.txt").exists());
    }

    #[test]
    fn new_folder_creates_the_folder_and_opens_inline_rename() {
        let d = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        assert!(w.apply_command(FileCommand::NewFolder));
        assert!(
            d.path().join("New Folder").is_dir(),
            "folder actually created"
        );
        assert!(w.is_renaming(), "new folder drops straight into rename");
        assert_eq!(w.rename_buffer(), "New Folder");
    }

    #[test]
    fn external_new_folder_uses_the_typed_name_without_rename_ui() {
        let d = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let doc = automerge::AutoCommit::new();
        assert!(
            Widget::invoke_resource_operation(
                &mut w,
                OperationKind::Effect,
                "files.new-folder",
                &serde_json::to_vec("Agent Created").unwrap(),
                &doc,
                &automerge::ROOT,
            )
            .unwrap()
        );
        assert!(d.path().join("Agent Created").is_dir());
        assert!(!w.is_renaming());
    }

    #[test]
    fn resource_open_refuses_a_deleted_or_replayed_entry_target() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("transient.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let doc = automerge::AutoCommit::new();
        let payload =
            serde_json::to_vec(&crate::search_provider::entry_resource_id(&file)).unwrap();

        std::fs::remove_file(&file).unwrap();
        w.refresh();
        let error = Widget::invoke_resource_operation(
            &mut w,
            OperationKind::View,
            "files.open",
            &payload,
            &doc,
            &automerge::ROOT,
        )
        .expect_err("a stale entry action must be refused");
        assert!(error.contains("no longer available"));
    }

    #[test]
    fn local_resource_actions_carry_only_opaque_ids_not_host_paths() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("private-plan.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let resources = Widget::resources(&w, &automerge::AutoCommit::new());
        let entry = resources[0]
            .children
            .iter()
            .find(|resource| resource.label == "private-plan.txt")
            .expect("the entry is exported");
        let open = entry
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("the entry exposes Open");
        let encoded = open
            .build_json(&serde_json::Map::new())
            .expect("Open takes no arguments");
        let payload: String = serde_json::from_slice(&encoded.payload).expect("string payload");
        assert_eq!(payload, entry.id.as_str());
        assert_ne!(payload, file.to_string_lossy());
        assert!(!String::from_utf8_lossy(&encoded.payload).contains("private-plan.txt"));
    }

    #[test]
    fn resource_catalogue_is_bounded_but_an_exact_visible_entry_resolves_lazily() {
        let d = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::new();
        w.current_path = d.path().to_path_buf();
        w.entries = (0..=MAX_EAGER_ENTRY_RESOURCES)
            .map(|index| {
                let name = format!("item-{index:03}.txt");
                let mut entry = ent(&name, false, "text/plain");
                entry.path = d.path().join(name);
                entry
            })
            .collect();
        let doc = automerge::AutoCommit::new();
        let catalogue = Widget::resources(&w, &doc);
        let eager_entries = catalogue[0]
            .children
            .iter()
            .filter(|resource| resource.kind == "files.entry")
            .count();
        assert_eq!(eager_entries, MAX_EAGER_ENTRY_RESOURCES);

        let last = w.entries.last().expect("fixture has a final entry");
        let id = FileExplorerWidget::entry_resource_id(last);
        let resolved = Widget::resource(&w, &doc, &id)
            .expect("an exact current-folder entry resolves on demand");
        assert_eq!(resolved.id.as_str(), id);
        assert!(resolved.actions.iter().any(|action| action.id == "open"));
        assert!(resolved.actions.iter().any(|action| action.id == "rename"));
    }

    #[test]
    fn paged_folder_root_preserves_the_canonical_folder_actions() {
        let d = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let doc = automerge::AutoCommit::new();
        let eager = Widget::resources(&w, &doc)
            .into_iter()
            .next()
            .expect("folder root is exported");
        let page = Widget::action_directory_resource_page(&w, &doc, &[], 1)
            .expect("Files supports bounded action-directory pages");
        let paged = page
            .resources
            .into_iter()
            .next()
            .expect("the first page contains the folder root");
        let resolved = Widget::resource(&w, &doc, eager.id.as_str())
            .expect("the canonical folder id resolves on demand");
        let ids = |resource: &Resource| {
            resource
                .actions
                .iter()
                .map(|action| action.id.clone())
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(paged.id, eager.id);
        assert_eq!(ids(&paged), ids(&eager));
        assert_eq!(ids(&resolved), ids(&eager));
    }

    #[test]
    fn entry_actions_fail_closed_when_the_operation_kind_is_replayed_as_view() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("keep.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let resource_id = crate::search_provider::entry_resource_id(&file);
        let payload = serde_json::to_vec(&vec!["trash", &resource_id]).unwrap();
        let error = Widget::invoke_resource_operation(
            &mut w,
            OperationKind::View,
            "files.entry-command",
            &payload,
            &automerge::AutoCommit::new(),
            &automerge::ROOT,
        )
        .expect_err("a destructive action cannot be replayed as a view");
        assert!(error.contains("invalid operation kind"));
        assert!(
            file.exists(),
            "the rejected action made no filesystem change"
        );
    }

    #[test]
    fn every_exported_local_file_handler_is_declared_for_the_broker() {
        let handlers = Widget::local_resource_operation_handlers(&FileExplorerWidget::new());
        for handler in [
            "files.refresh",
            "files.new-folder",
            "files.paste",
            "files.new-from-template",
            "files.open",
            "files.reveal-record",
            "files.folder-command",
            "files.entry-command",
            "files.rename",
            "files.share-link",
            "files.remote-account",
            "files.remote-dav",
        ] {
            assert!(handlers.contains(&handler), "missing handler {handler}");
        }
    }

    #[test]
    fn exported_rename_changes_case_without_overwriting_another_entry() {
        use ui::tilo_actions::{ActionAddress, door};
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("report.txt");
        std::fs::write(&original, b"source content").unwrap();
        let occupied = directory.path().join("occupied.txt");
        std::fs::write(&occupied, b"keep occupied content").unwrap();
        let mut files = FileExplorerWidget::with_persistence(None, Some(directory.path().into()));
        files.navigate(directory.path().into());
        let doc = automerge::AutoCommit::new();
        let mut invoke_rename = |source: &Path, name: &str| {
            let resources = Widget::resources(&files, &doc);
            let address =
                ActionAddress::new(crate::search_provider::entry_resource_id(source), "rename");
            let arguments = serde_json::Map::from_iter([(
                "value".into(),
                serde_json::Value::String(name.into()),
            )]);
            let encoded = door::encode_json(&resources, &address, &arguments).unwrap();
            Widget::invoke_resource_operation(
                &mut files,
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &automerge::ROOT,
            )
        };
        assert!(invoke_rename(&original, "REPORT.txt").unwrap());
        let renamed = directory.path().join("REPORT.txt");
        assert_eq!(std::fs::read(&renamed).unwrap(), b"source content");
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .any(|entry| entry.unwrap().file_name() == "REPORT.txt")
        );
        assert!(invoke_rename(&renamed, "occupied.txt").is_err());
        assert_eq!(std::fs::read(&renamed).unwrap(), b"source content");
        assert_eq!(std::fs::read(&occupied).unwrap(), b"keep occupied content");
        // Sharing file identity does not make another directory entry the
        // spelling being renamed. The action must preserve both hard links.
        let hardlink = directory.path().join("hardlink.txt");
        std::fs::hard_link(&renamed, &hardlink).unwrap();
        assert!(invoke_rename(&renamed, "hardlink.txt").is_err());
        assert_eq!(std::fs::read(&renamed).unwrap(), b"source content");
        assert_eq!(std::fs::read(&hardlink).unwrap(), b"source content");
        #[cfg(unix)]
        {
            let dangling = directory.path().join("dangling.txt");
            std::os::unix::fs::symlink("missing-target", &dangling).unwrap();
            assert!(invoke_rename(&renamed, "dangling.txt").is_err());
            assert_eq!(
                std::fs::read_link(&dangling).unwrap(),
                PathBuf::from("missing-target")
            );
            assert_eq!(std::fs::read(&renamed).unwrap(), b"source content");
        }
    }

    #[test]
    fn entry_resource_uses_a_wire_safe_id_and_renames_through_the_action_door() {
        use ui::tilo_actions::{ActionAddress, door};

        let d = tempfile::tempdir().unwrap();
        // A control separator is a legal Unix filename byte but must not leak
        // into the resource/action wire address.
        // Windows forbids control bytes in physical filenames. Keep the
        // separator in the presented resource name on both platforms.
        let old = d.path().join(if cfg!(windows) {
            "old.txt"
        } else {
            "old\u{1f}.txt"
        });
        std::fs::write(&old, b"x").unwrap();
        let record_id = uuid::Uuid::from_u128(0xfeed);
        let mut w = FileExplorerWidget::new();
        w.workspace_roots = vec![d.path().to_path_buf()];
        w.current_path = d.path().to_path_buf();
        let mut entry = ent("old\u{1f}.txt", false, "text/plain");
        entry.path = old.clone();
        entry.record_id = Some(*record_id.as_bytes());
        w.entries = vec![entry];
        let doc = automerge::AutoCommit::new();
        let resources = Widget::resources(&w, &doc);
        let entry = resources[0]
            .children
            .iter()
            .find(|resource| {
                resource.id.as_str() == crate::search_provider::record_resource_id(record_id)
            })
            .expect("a collaborative file uses its record UUID as the resource identity");
        assert_eq!(entry.kind, "files.record");
        let address = ActionAddress::new(entry.id.as_str(), "rename");
        assert_eq!(
            ActionAddress::parse(&address.encode()),
            Some(address.clone())
        );
        let mut arguments = serde_json::Map::new();
        arguments.insert("value".into(), serde_json::Value::String("new.txt".into()));
        let encoded = door::encode_json(&resources, &address, &arguments)
            .expect("the entry's prompted rename action encodes");
        assert!(
            !door::resolve(&resources, &address)
                .expect("rename action is exported")
                .contract
                .reversible,
            "an external filesystem rename has no Tilo action-history inverse"
        );
        assert!(
            Widget::invoke_resource_operation(
                &mut w,
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &automerge::ROOT,
            )
            .unwrap()
        );
        assert!(!old.exists());
        let renamed = d.path().join("new.txt");
        assert!(renamed.exists());
        // A mount refresh presents a new path but the same daemon-provided
        // record identity, so the old action address resolves against it.
        w.entries[0].path = renamed;
        w.entries[0].name = "new.txt".into();
        w.entries[0].record_id = Some(*record_id.as_bytes());
        let refreshed = Widget::resources(&w, &doc);
        assert!(
            door::resolve(&refreshed, &address).is_some(),
            "the record action address survives a path rename and resolves its fresh path"
        );
    }

    #[test]
    fn exported_background_operations_have_valid_job_contracts() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("shared.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let resources = Widget::resources(&w, &automerge::AutoCommit::new());
        for action in resources.iter().flat_map(|resource| {
            resource.actions.iter().chain(
                resource
                    .children
                    .iter()
                    .flat_map(|child| child.actions.iter()),
            )
        }) {
            if matches!(action.contract.execution, ExecutionMode::Background { .. }) {
                assert_eq!(action.contract.result, ResultSpec::Job, "{}", action.id);
                assert!(
                    action.contract.validate_declaration().is_empty(),
                    "{}",
                    action.id
                );
            }
        }
    }

    #[test]
    fn copy_then_paste_duplicates_the_file_into_the_current_dir() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(src.clone());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "a.txt")
            .unwrap();
        w.select(Some(idx));
        assert!(w.apply_command(FileCommand::Copy));
        w.navigate(dest.clone());
        assert!(w.apply_command(FileCommand::Paste));
        assert!(dest.join("a.txt").exists(), "copied into dest");
        assert!(src.join("a.txt").exists(), "original kept (copy, not move)");
    }

    #[test]
    fn cut_then_paste_moves_the_file_into_the_current_dir() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("m.txt"), b"x").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(src.clone());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "m.txt")
            .unwrap();
        w.select(Some(idx));
        assert!(w.apply_command(FileCommand::Cut));
        w.navigate(dest.clone());
        assert!(w.apply_command(FileCommand::Paste));
        assert!(dest.join("m.txt").exists(), "moved into dest");
        assert!(!src.join("m.txt").exists(), "original removed (move)");
    }

    #[test]
    fn partial_cut_paste_retains_only_the_sources_that_need_retrying() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let moved = src.join("moved.txt");
        let vanished = src.join("vanished.txt");
        std::fs::write(&moved, b"x").unwrap();
        std::fs::write(&vanished, b"x").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(src);
        w.selection.select_all(w.displayed_entries().len());
        assert!(w.apply_command(FileCommand::Cut));
        std::fs::remove_file(&vanished).unwrap();

        w.navigate(dest.clone());
        assert!(w.apply_command(FileCommand::Paste));

        assert!(dest.join("moved.txt").exists(), "the available item moved");
        assert_eq!(
            w.clipboard,
            vec![vanished],
            "retry must contain only the item whose move actually failed"
        );
        assert!(w.clipboard_cut, "the remaining item keeps cut semantics");
    }

    fn invoke_transfer_action(
        files: &mut FileExplorerWidget,
        source: Option<&Path>,
        action: &str,
    ) -> Result<bool, String> {
        use ui::tilo_actions::{ActionAddress, door};
        let doc = automerge::AutoCommit::new();
        let resources = Widget::resources(files, &doc);
        let resource = source.map_or_else(
            || resources.first().unwrap().id.as_str().to_owned(),
            crate::search_provider::entry_resource_id,
        );
        let encoded = door::encode_json(
            &resources,
            &ActionAddress::new(resource, action),
            &serde_json::Map::new(),
        )
        .expect("the current resource must export the transfer action");
        Widget::invoke_resource_operation(
            files,
            OperationKind::Effect,
            &encoded.type_id,
            &encoded.payload,
            &doc,
            &automerge::ROOT,
        )
    }

    #[cfg(unix)]
    #[test]
    fn exported_paste_preserves_dangling_destination_links() {
        for cut in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("report.txt");
            let destination = directory.path().join("destination");
            let outside = directory.path().join("unselected-target.txt");
            std::fs::write(&source, b"selected bytes").unwrap();
            std::fs::create_dir(&destination).unwrap();
            let occupied = destination.join("report.txt");
            let numbered = destination.join("report 2.txt");
            std::os::unix::fs::symlink(&outside, &occupied).unwrap();
            std::os::unix::fs::symlink(&outside, &numbered).unwrap();
            let mut files =
                FileExplorerWidget::with_persistence(None, Some(directory.path().into()));
            files.navigate(directory.path().into());
            assert!(
                invoke_transfer_action(&mut files, Some(&source), if cut { "cut" } else { "copy" })
                    .unwrap()
            );
            files.navigate(destination.clone());
            assert!(invoke_transfer_action(&mut files, None, "paste").unwrap());

            assert_eq!(std::fs::read_link(&occupied).unwrap(), outside);
            assert_eq!(std::fs::read_link(&numbered).unwrap(), outside);
            assert!(
                !outside.exists(),
                "Paste must not write to either link target"
            );
            assert_eq!(
                std::fs::read(destination.join("report 3.txt")).unwrap(),
                b"selected bytes"
            );
            assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 3);
            assert_eq!(source.exists(), !cut);
            if !cut {
                assert_eq!(std::fs::read(&source).unwrap(), b"selected bytes");
            }
            assert_eq!(files.clipboard.is_empty(), cut);
            assert!(
                files
                    .displayed_entries()
                    .iter()
                    .any(|entry| entry.name == "report 3.txt")
            );
        }
    }

    #[test]
    fn exported_cut_paste_reports_destination_failure_and_keeps_retryable_source() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("report.txt");
        let destination = directory.path().join("destination");
        std::fs::write(&source, b"selected bytes").unwrap();
        std::fs::create_dir(&destination).unwrap();
        let mut files = FileExplorerWidget::with_persistence(None, Some(directory.path().into()));
        files.navigate(directory.path().into());
        assert!(invoke_transfer_action(&mut files, Some(&source), "cut").unwrap());
        files.navigate(destination.clone());
        // The folder disappears after selection. A file now occupies that
        // entry: failure must preserve both objects and keep Cut retryable.
        std::fs::remove_dir(&destination).unwrap();
        std::fs::write(&destination, b"replacement bytes").unwrap();
        assert!(invoke_transfer_action(&mut files, None, "paste").is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"selected bytes");
        assert_eq!(std::fs::read(&destination).unwrap(), b"replacement bytes");
        assert_eq!(files.clipboard, vec![source.clone()]);
        assert!(files.clipboard_cut);

        // Restore only this owned fixture directory and use the same public
        // Paste action again, without rebuilding the selection or clipboard.
        std::fs::remove_file(&destination).unwrap();
        std::fs::create_dir(&destination).unwrap();
        assert!(invoke_transfer_action(&mut files, None, "paste").unwrap());
        assert_eq!(
            std::fs::read(destination.join("report.txt")).unwrap(),
            b"selected bytes"
        );
        assert!(!source.exists());
        assert!(files.clipboard.is_empty());
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);
    }

    #[test]
    fn history_back_and_forward_traverse_visited_dirs() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir(&a).unwrap();
        let b = d.path().join("b");
        std::fs::create_dir(&b).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        w.navigate(a.clone());
        w.navigate(b.clone());
        assert_eq!(w.current_path(), b);
        w.history_back();
        assert_eq!(w.current_path(), a, "back to the previously-visited dir");
        w.history_back();
        assert_eq!(w.current_path(), d.path());
        w.history_forward();
        assert_eq!(w.current_path(), a, "forward re-visits");
    }

    /// A hermetic widget whose favourites/zoom persist to a throwaway dir, so
    /// tests never touch the user's real `~/.config`.
    fn hermetic() -> (FileExplorerWidget, tempfile::TempDir) {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        (w, cfg)
    }

    #[test]
    fn nearby_receiver_actions_are_opaque_confirmed_and_current() {
        use ui::collaboration::{
            NearbyDestinationHandle, NearbyTransferDestination, NearbyTransferDirection,
            NearbyTransferHandle, NearbyTransferPhase, NearbyTransferSnapshot,
            NearbyTransferStatus,
        };
        use ui::tilo_actions::ActionInput;

        let transfer = NearbyTransferHandle::from_issued_bytes([0x41; 16]);
        let destination = NearbyDestinationHandle::from_issued_bytes([0x42; 16]);
        let mut snapshot = NearbyTransferSnapshot {
            destinations: vec![NearbyTransferDestination {
                handle: destination,
                label: "Workspace files".into(),
                remembered_for_session: false,
            }],
            transfers: vec![NearbyTransferStatus {
                handle: transfer,
                name: "Photos".into(),
                device_label: "Travel laptop".into(),
                direction: NearbyTransferDirection::Receiving,
                phase: NearbyTransferPhase::AwaitingConsent,
                transferred_bytes: 0,
                total_bytes: 42,
            }],
            ..Default::default()
        };
        let (mut widget, _config) = hermetic();
        widget
            .nearby_send
            .install_snapshot_for_test(snapshot.clone());
        let root = widget.action_directory_folder_root();
        let transfer_resource = root
            .children
            .iter()
            .find(|resource| resource.kind == "files.nearby-transfer")
            .expect("incoming transfer resource");
        assert!(!transfer_resource.id.as_str().contains("41414141"));
        let accept = transfer_resource
            .actions
            .iter()
            .find(|action| action.id.starts_with("accept-"))
            .expect("accept action");
        assert_eq!(accept.contract.confirmation, ConfirmationPolicy::Confirm);
        assert!(!accept.id.contains("42424242"));
        let ActionInput::Immediate(encoded) = &accept.input else {
            panic!("accept is nullary over an app-owned opaque payload")
        };

        snapshot.transfers.clear();
        widget.nearby_send.install_snapshot_for_test(snapshot);
        let document = automerge::AutoCommit::new();
        assert!(
            Widget::invoke_resource_operation(
                &mut widget,
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
                &document,
                &automerge::ROOT,
            )
            .expect_err("stale receiver decision must fail closed")
            .contains("no longer available")
        );
        assert!(widget.nearby_send.take_requests().is_empty());
    }

    #[test]
    fn double_click_click_command_navigates_into_a_directory() {
        // Drives the real pointer path: a `Click` command, then a second within
        // the double-click window → the app opens (navigates into) the folder.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let (mut w, _cfg) = hermetic();
        w.navigate(d.path().to_path_buf());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "sub")
            .unwrap();

        w.apply_command(FileCommand::Click {
            index: idx,
            ctrl: false,
            shift: false,
        });
        assert_eq!(w.current_path(), d.path(), "first click only selects");
        assert_eq!(w.selected_index(), Some(idx));

        w.apply_command(FileCommand::Click {
            index: idx,
            ctrl: false,
            shift: false,
        });
        assert_eq!(
            w.current_path(),
            d.path().join("sub"),
            "double-click opens the directory"
        );
    }

    #[test]
    fn zoom_commands_step_the_item_size_and_clamp() {
        let (mut w, _cfg) = hermetic();
        let base = w.zoom_level();
        w.apply_command(FileCommand::ZoomIn);
        assert_eq!(w.zoom_level(), base + 1, "zoom in raises the step");
        w.apply_command(FileCommand::ZoomOut);
        w.apply_command(FileCommand::ZoomOut);
        assert_eq!(w.zoom_level(), base - 1, "zoom out lowers the step");
        // Clamps at the extremes.
        for _ in 0..50 {
            w.apply_command(FileCommand::ZoomOut);
        }
        assert_eq!(
            w.zoom_level(),
            config::ZOOM_MIN,
            "zoom clamps at the minimum"
        );
    }

    #[test]
    fn zoom_level_persists_across_reconstruction() {
        let cfg = tempfile::tempdir().unwrap();
        {
            let mut w = FileExplorerWidget::with_persistence(
                Some(cfg.path().to_path_buf()),
                Some(PathBuf::from("/home/tester")),
            );
            w.apply_command(FileCommand::ZoomIn);
            w.apply_command(FileCommand::ZoomIn);
            assert_eq!(w.zoom_level(), 2);
        }
        // A fresh widget reads the persisted level back.
        let w2 = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        assert_eq!(w2.zoom_level(), 2, "zoom level survives a restart");
    }

    #[test]
    fn favorite_commands_add_reorder_and_remove_and_persist() {
        let cfg = tempfile::tempdir().unwrap();
        let make = || {
            FileExplorerWidget::with_persistence(
                Some(cfg.path().to_path_buf()),
                Some(PathBuf::from("/home/tester")),
            )
        };
        let sub = PathBuf::from("/home/tester/Projects/tilo");
        let mut w = make();
        let before = w.favorites().len();
        w.apply_command(FileCommand::AddFavorite(sub.clone()));
        assert_eq!(w.favorites().len(), before + 1, "added one favourite");
        assert_eq!(w.favorites().last().unwrap().path, sub);

        // Reorder: drag the new favourite (last) up to index 1.
        let last = w.favorites().len() - 1;
        w.apply_command(FileCommand::ReorderFavorite { from: last, to: 1 });
        assert_eq!(w.favorites()[1].path, sub, "reorder moved it to slot 1");

        // The new order + membership persists to a fresh widget.
        let w2 = make();
        assert_eq!(w2.favorites()[1].path, sub, "reordered favourite persists");

        // Remove drops it (and persists).
        let mut w3 = make();
        w3.apply_command(FileCommand::RemoveFavorite(sub.clone()));
        assert!(!w3.favorites().iter().any(|f| f.path == sub));
        let w4 = make();
        assert!(
            !w4.favorites().iter().any(|f| f.path == sub),
            "removal persists"
        );
    }

    #[test]
    fn favourites_follow_localized_xdg_user_dirs() {
        use crate::favorites::{FavKind, UserDirs};
        let cfg = tempfile::tempdir().unwrap();
        let home = PathBuf::from("/home/tester");
        // A German desktop: the XDG user dirs resolve to localized folder names.
        let dirs = UserDirs {
            home: Some(home.clone()),
            documents: Some(home.join("Dokumente")),
            downloads: Some(home.join("Downloads")),
            pictures: Some(home.join("Bilder")),
            music: Some(home.join("Musik")),
            videos: Some(home.join("Videos")),
            ..Default::default()
        };
        let w = FileExplorerWidget::with_persistence_dirs(
            Some(cfg.path().to_path_buf()),
            Some(home.clone()),
            dirs,
        );
        let labels: Vec<String> = w.favorites().iter().map(|f| f.label.clone()).collect();
        // The sidebar shows the user's real localized names, not hard-coded
        // English ones.
        assert!(
            labels.contains(&"Dokumente".to_string()),
            "labels: {labels:?}"
        );
        assert!(labels.contains(&"Bilder".to_string()), "labels: {labels:?}");
        assert!(labels.contains(&"Musik".to_string()), "labels: {labels:?}");
        assert!(
            !labels.contains(&"Documents".to_string()),
            "must not show hard-coded English Documents"
        );
        // The Documents favourite points at the actual localized directory (so
        // clicking it opens ~/Dokumente, where the files really are) and keeps
        // its evocative glyph despite the localized label.
        let docs = w
            .favorites()
            .into_iter()
            .find(|f| f.kind == FavKind::Documents)
            .unwrap();
        assert_eq!(docs.path, home.join("Dokumente"));
        assert_eq!(docs.icon(), ui::icons::Icon::FileText);
    }

    #[test]
    fn new_from_template_creates_the_file_and_opens_rename() {
        let d = tempfile::tempdir().unwrap();
        let tmpl = d.path().join("Doc.md");
        std::fs::write(&tmpl, b"template body").unwrap();
        let work = d.path().join("work");
        std::fs::create_dir(&work).unwrap();

        let (mut w, _cfg) = hermetic();
        w.navigate(work.clone());
        w.apply_command(FileCommand::NewFromTemplate(tmpl));
        assert!(
            work.join("Doc.md").exists(),
            "template copied into the current dir"
        );
        assert_eq!(
            std::fs::read(work.join("Doc.md")).unwrap(),
            b"template body"
        );
        assert!(
            w.is_renaming(),
            "new file drops straight into inline-rename"
        );
    }

    #[test]
    fn toggle_hidden_reveals_and_hides_dotfiles() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".secret"), b"x").unwrap();
        std::fs::write(d.path().join("visible.txt"), b"x").unwrap();
        let (mut w, _cfg) = hermetic();
        w.navigate(d.path().to_path_buf());
        assert!(
            !w.entries_snapshot().iter().any(|e| e.name == ".secret"),
            "dotfiles hidden by default"
        );
        w.apply_command(FileCommand::ToggleHidden);
        assert!(w.show_hidden());
        assert!(
            w.entries_snapshot().iter().any(|e| e.name == ".secret"),
            "dotfiles shown after toggle"
        );
        w.apply_command(FileCommand::ToggleHidden);
        assert!(
            !w.entries_snapshot().iter().any(|e| e.name == ".secret"),
            "toggled back off"
        );
    }

    /// A file manager pointed at a directory that has files in it lists
    /// those files.
    ///
    /// Asserts the *exact* set rather than "contains one of them". The
    /// frame this guards showed "This folder is empty" over a directory
    /// that held five entries, and an `.iter().any(|e| e.name == …)`
    /// probe for a single name — the shape every other listing test in
    /// this file uses — would still have passed on a listing that had
    /// lost the other four.
    ///
    /// The empty-state banner is a draw-only side effect with no
    /// accessor, so the assertions are on the two conditions that gate
    /// it (a non-empty `entries`, no error) plus the item count the same
    /// view renders in its footer.
    #[test]
    fn a_populated_directory_lists_exactly_its_contents() {
        let d = tempfile::tempdir().unwrap();
        let names = [
            "alpha.txt",
            "beta.md",
            "delta.rs",
            "epsilon.png",
            "gamma.odt",
        ];
        for n in names {
            std::fs::write(d.path().join(n), b"x").unwrap();
        }

        let (mut w, _cfg) = hermetic();
        w.navigate(d.path().to_path_buf());

        assert_eq!(
            w.error_message(),
            None,
            "a directory that reads cleanly reports no error",
        );

        let mut listed: Vec<String> = w
            .entries_snapshot()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        listed.sort();
        let mut expected: Vec<String> = names.iter().map(|s| (*s).to_string()).collect();
        expected.sort();
        assert_eq!(
            listed, expected,
            "every file in the directory is listed, and nothing else is",
        );

        let (count, _path) = w.footer_text();
        assert_eq!(
            count,
            names.len(),
            "the footer counts the same entries the list shows",
        );
    }

    #[test]
    fn scroll_position_survives_a_structural_rebuild_but_resets_on_navigation() {
        // The real fix: selecting a row / re-sorting rebuilds the whole
        // composition (a fresh Scrollable), which used to snap the list back to
        // the top. The persisted offset cell — the exact one the rebuilt
        // Scrollable binds to via `with_persisted_offset` — must carry the
        // position across those rebuilds, and only reset when the directory
        // actually changes.
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir(&a).unwrap();
        for i in 0..6 {
            std::fs::write(a.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let b = d.path().join("b");
        std::fs::create_dir(&b).unwrap();

        let (mut w, _cfg) = hermetic();
        w.navigate(a);
        // Simulate the list having been scrolled down 240px.
        w.scroll_offset.set((0.0, 240.0));

        // Selecting a row is a structural rebuild — scroll must hold.
        w.apply_command(FileCommand::Click {
            index: 0,
            ctrl: false,
            shift: false,
        });
        assert!(
            (w.scroll_offset.get().1 - 240.0).abs() <= f32::EPSILON,
            "selecting a row keeps the scroll"
        );
        // Re-sorting is a structural rebuild — scroll must hold.
        w.apply_command(FileCommand::SetSort(SortBy::Size));
        assert!(
            (w.scroll_offset.get().1 - 240.0).abs() <= f32::EPSILON,
            "re-sorting keeps the scroll"
        );

        // Navigating to a different directory starts at the top.
        w.navigate(b);
        assert_eq!(
            w.scroll_offset.get(),
            (0.0, 0.0),
            "a new directory scrolls to the top"
        );
    }

    #[test]
    fn context_select_targets_the_clicked_row_not_a_stale_selection() {
        // Guards the dangerous bug: a right-click's menu must act on the row it
        // landed on, not whatever happened to be selected. `ContextSelect` is
        // the command the content pane pushes on a right-press.
        let (mut w, _cfg) = hermetic();
        w.entries = vec![
            ent("a.txt", false, "text/plain"),
            ent("b.txt", false, "text/plain"),
            ent("c.txt", false, "text/plain"),
        ];
        // Stale selection on row 0.
        w.selection.select_only(0);
        // Right-press on row 2 moves the selection there.
        assert!(w.apply_command(FileCommand::ContextSelect(Some(2))));
        assert_eq!(
            w.selected_indices(),
            vec![2],
            "context select targets the clicked row"
        );

        // A right-press inside an existing multi-selection preserves all of it
        // (so a menu action applies to every highlighted item).
        w.selection.select_only(1);
        w.selection.toggle(2);
        w.apply_command(FileCommand::ContextSelect(Some(1)));
        let mut got = w.selected_indices();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![1, 2],
            "right-click within a multi-selection keeps it"
        );

        // A right-press on empty space clears the selection.
        w.apply_command(FileCommand::ContextSelect(None));
        assert!(
            w.selected_indices().is_empty(),
            "empty-space right-click clears selection"
        );
    }

    #[test]
    fn context_menu_gates_new_file_on_templates_existing() {
        // The "New File ▸" entry appears iff the Templates dir has files, and
        // "New Folder" is always present. Templates are injected so the check is
        // hermetic (no reliance on the ambient Templates dir).
        let (w, _cfg) = hermetic();
        let has_new_file = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label.starts_with("New File"))
        };
        let has_new_folder = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label == "New Folder")
        };

        // Empty Templates dir → no "New File", but "New Folder" stays.
        let none = w.context_menu_spec(&[]);
        assert!(!has_new_file(&none), "no templates → submenu hidden");
        assert!(has_new_folder(&none), "New Folder is always offered");

        // Two templates → the "New File ▸" entry appears.
        let two = w.context_menu_spec(&[PathBuf::from("Doc.md"), PathBuf::from("Sheet.csv")]);
        assert!(
            has_new_file(&two),
            "templates present → New File entry shown"
        );
        assert!(has_new_folder(&two));
    }

    #[test]
    fn disappearing_templates_do_not_leave_an_empty_flyout() {
        let (mut w, _cfg) = hermetic();
        // A catalogue can be emptied after the parent menu opens. The flyout
        // must close instead of painting an empty, focusable surface.
        w.open_templates_submenu(Point::new(80.0, 80.0));
        assert!(!w.submenu.is_open());
        assert!(w.submenu_commands.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn collaborative_selection_offers_and_applies_keep_offline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shared.txt");
        std::fs::write(&file, b"shared").unwrap();
        let availability = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_version": null,
            "residency": {"state": "not_resident"},
            "transfer": {"phase": "idle", "progress": null, "last_error": null,
                "last_error_at_ms": null, "error_truncated": false},
            "policy": "automatic",
            "replicas": {"state": "unknown"},
            "freshness": {"metadata_observed_at_ms": null, "local_verified_at_ms": null,
                "replicas_observed_at_ms": null}
        }))
        .unwrap();
        xattr::set(&file, crate::model::AVAILABILITY_XATTR, &availability).unwrap();

        let mut w = FileExplorerWidget::new();
        w.current_path = dir.path().to_path_buf();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = file.clone();
        entry.availability = crate::model::read_availability(&file);
        w.entries = vec![entry];
        w.selection.select_only(0);
        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Keep Offline" && *command == Some(FileCommand::KeepOffline)
        }));
        assert!(w.apply_command(FileCommand::KeepOffline));
        assert_eq!(
            xattr::get(&file, crate::model::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice())
        );
    }

    #[test]
    #[cfg(unix)]
    fn free_local_space_is_offered_and_sent_only_for_eligible_local_files() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.txt");
        let remote = dir.path().join("remote.txt");
        std::fs::write(&local, b"local").unwrap();
        std::fs::write(&remote, b"remote").unwrap();
        let availability = |state: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": uuid::Uuid::new_v4(),
                "current_version": null,
                "residency": state,
                "transfer": {"phase": "idle", "progress": null, "last_error": null,
                    "last_error_at_ms": null, "error_truncated": false},
                "policy": "automatic",
                "policy_inherited": false,
                "replicas": {"state": "unknown"},
                "freshness": {"metadata_observed_at_ms": null, "local_verified_at_ms": null,
                    "replicas_observed_at_ms": null}
            }))
            .unwrap()
        };
        xattr::set(
            &local,
            crate::model::AVAILABILITY_XATTR,
            &availability(serde_json::json!({"state": "resident", "bytes": 5})),
        )
        .unwrap();
        xattr::set(
            &remote,
            crate::model::AVAILABILITY_XATTR,
            &availability(serde_json::json!({"state": "not_resident"})),
        )
        .unwrap();

        let mut widget = FileExplorerWidget::new();
        widget.current_path = dir.path().to_path_buf();
        widget.entries = [("local.txt", &local), ("remote.txt", &remote)]
            .into_iter()
            .map(|(name, path)| {
                let mut entry = ent(name, false, "text/plain");
                entry.path = path.clone();
                entry.availability = crate::model::read_availability(path);
                entry
            })
            .collect();
        widget.selection.select_all(2);

        assert!(
            widget
                .context_menu_spec(&[])
                .iter()
                .any(|(label, command)| {
                    label == "Free Local Space" && *command == Some(FileCommand::FreeLocalSpace)
                })
        );
        assert!(widget.apply_command(FileCommand::FreeLocalSpace));
        assert_eq!(
            xattr::get(&local, crate::model::AVAILABILITY_CONTROL_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"evict".as_slice())
        );
        assert_eq!(
            xattr::get(&remote, crate::model::AVAILABILITY_CONTROL_XATTR).unwrap(),
            None
        );
    }

    #[test]
    #[cfg(unix)]
    fn scheduled_metadata_refresh_preserves_record_selection_and_scroll() {
        let dir = tempfile::tempdir().unwrap();
        // Production discovers this from the mounted workspace root. Mark the
        // isolated fixture as a collaborative root so the listing reads the
        // record/sync xattrs whose refresh behavior this test exercises.
        let previous_roots = std::env::var_os("TILO_WORKSPACE_ROOTS");
        unsafe { std::env::set_var("TILO_WORKSPACE_ROOTS", dir.path()) };
        let before = dir.path().join("before.txt");
        let after = dir.path().join("after.txt");
        std::fs::write(&before, b"shared").unwrap();
        let record = uuid::Uuid::from_u128(0x5151);
        xattr::set(&before, crate::model::RECORD_ID_XATTR, record.as_bytes()).unwrap();
        xattr::set(&before, crate::model::SYNC_XATTR, b"projected").unwrap();

        let mut widget = FileExplorerWidget::new();
        widget.load(dir.path().to_path_buf());
        widget.selection.select_only(0);
        widget.scroll_offset.set((0.0, 137.0));
        std::fs::rename(&before, &after).unwrap();
        xattr::set(&after, crate::model::SYNC_XATTR, b"local").unwrap();
        widget.next_metadata_refresh = Instant::now();

        assert!(widget.refresh_metadata_if_due(Instant::now()));
        assert_eq!(widget.entries[0].path, after);
        assert_eq!(
            widget.entries[0].sync_state,
            Some(crate::model::SyncState::Local)
        );
        assert_eq!(widget.selection.selected(), vec![0]);
        assert_eq!(widget.selection.primary(), Some(0));
        assert_eq!(widget.scroll_offset.get(), (0.0, 137.0));
        if let Some(previous_roots) = previous_roots {
            unsafe { std::env::set_var("TILO_WORKSPACE_ROOTS", previous_roots) };
        } else {
            unsafe { std::env::remove_var("TILO_WORKSPACE_ROOTS") };
        }
    }

    #[test]
    #[cfg(unix)]
    fn mixed_selection_exposes_both_storage_policies_and_writes_only_eligible_records() {
        let dir = tempfile::tempdir().unwrap();
        let automatic = dir.path().join("automatic.txt");
        let pinned = dir.path().join("pinned.txt");
        let ordinary = dir.path().join("ordinary.txt");
        for path in [&automatic, &pinned, &ordinary] {
            std::fs::write(path, b"data").unwrap();
        }
        let availability = |policy: &str, inherited: bool| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_version": null,
                "residency": {"state": "resident", "bytes": 4},
                "transfer": {"phase": "idle", "progress": null, "last_error": null,
                    "last_error_at_ms": null, "error_truncated": false},
                "policy": policy,
                "policy_inherited": inherited,
                "replicas": {"state": "unknown"},
                "freshness": {"metadata_observed_at_ms": null, "local_verified_at_ms": null,
                    "replicas_observed_at_ms": null}
            }))
            .unwrap()
        };
        xattr::set(
            &automatic,
            crate::model::AVAILABILITY_XATTR,
            &availability("automatic", false),
        )
        .unwrap();
        xattr::set(
            &pinned,
            crate::model::AVAILABILITY_XATTR,
            &availability("pinned", false),
        )
        .unwrap();

        let mut w = FileExplorerWidget::new();
        w.current_path = dir.path().to_path_buf();
        w.entries = [
            ("automatic.txt", &automatic),
            ("pinned.txt", &pinned),
            ("ordinary.txt", &ordinary),
        ]
        .into_iter()
        .map(|(name, path)| {
            let mut entry = ent(name, false, "text/plain");
            entry.path = path.clone();
            entry.availability = crate::model::read_availability(path);
            entry
        })
        .collect();
        w.selection.select_all(3);

        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Keep Offline" && *command == Some(FileCommand::KeepOffline)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Use Automatic Storage" && *command == Some(FileCommand::RemoveOfflinePin)
        }));

        w.apply_command(FileCommand::KeepOffline);
        assert_eq!(
            xattr::get(&automatic, crate::model::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice())
        );
        assert_eq!(xattr::get(&pinned, crate::model::PIN_XATTR).unwrap(), None);
        assert_eq!(
            xattr::get(&ordinary, crate::model::PIN_XATTR).unwrap(),
            None
        );

        // `refresh` deliberately treats this temp directory as an ordinary
        // non-workspace path. Restore the directly-read projections so the
        // second half can exercise the opposite command independently.
        for entry in &mut w.entries {
            entry.availability = crate::model::read_availability(&entry.path);
        }
        w.selection.select_all(w.entries.len());
        w.apply_command(FileCommand::RemoveOfflinePin);
        assert_eq!(
            xattr::get(&pinned, crate::model::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"automatic".as_slice())
        );
        assert_eq!(
            xattr::get(&automatic, crate::model::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice()),
            "the opposite policy action must not rewrite an automatic record"
        );
        assert_eq!(
            xattr::get(&ordinary, crate::model::PIN_XATTR).unwrap(),
            None
        );
    }

    #[test]
    #[cfg(unix)]
    fn inherited_pin_is_explained_but_not_offered_as_a_local_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("inherited.txt");
        std::fs::write(&file, b"data").unwrap();
        let availability = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_version": null,
            "residency": {"state": "resident", "bytes": 4},
            "transfer": {"phase": "idle", "progress": null, "last_error": null,
                "last_error_at_ms": null, "error_truncated": false},
            "policy": "pinned",
            "policy_inherited": true,
            "replicas": {"state": "unknown"},
            "freshness": {"metadata_observed_at_ms": null, "local_verified_at_ms": null,
                "replicas_observed_at_ms": null}
        }))
        .unwrap();
        xattr::set(&file, crate::model::AVAILABILITY_XATTR, &availability).unwrap();

        let mut w = FileExplorerWidget::new();
        w.current_path = dir.path().to_path_buf();
        let mut entry = ent("inherited.txt", false, "text/plain");
        entry.path = file.clone();
        entry.availability = crate::model::read_availability(&file);
        w.entries = vec![entry];
        w.selection.select_only(0);

        let spec = w.context_menu_spec(&[]);
        assert!(
            spec.iter().any(|(label, command)| {
                label == "Pinned by Parent Folder" && command.is_none()
            })
        );
        assert!(
            !spec
                .iter()
                .any(|(_, command)| { *command == Some(FileCommand::RemoveOfflinePin) })
        );
        w.apply_command(FileCommand::RemoveOfflinePin);
        assert_eq!(xattr::get(&file, crate::model::PIN_XATTR).unwrap(), None);
    }

    #[test]
    fn failed_projected_file_offers_retry_and_availability_details() {
        let mut entry = ent("offline.txt", false, "text/plain");
        entry.availability = crate::model::AvailabilityProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_version": null,
                "residency": {"state": "not_resident"},
                "transfer": {"phase": "idle", "progress": null,
                    "last_error": "No available replica could provide this file",
                    "last_error_at_ms": 12, "error_truncated": false},
                "policy": "automatic",
                "replicas": {"state": "known", "reachable": 0, "durable": 1},
                "freshness": {"metadata_observed_at_ms": 10,
                    "local_verified_at_ms": null, "replicas_observed_at_ms": 11}
            }))
            .unwrap(),
        );
        let mut w = FileExplorerWidget::new();
        w.entries = vec![entry];
        w.selection.select_only(0);

        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Retry Download" && *command == Some(FileCommand::RetryTransfer)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Availability…" && *command == Some(FileCommand::ShowAvailability)
        }));

        w.apply_command(FileCommand::ShowAvailability);
        let panel = w.collab_panel.as_ref().expect("availability panel");
        assert!(panel.summary.contains("Not stored"));
        assert!(panel.lines.iter().any(|line| line.contains("durable")));
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("No available replica"))
        );
    }

    #[test]
    fn authority_sharing_and_history_open_real_detail_panels() {
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.record_id =
            Some(*uuid::Uuid::from_u128(0xb0d4_e23f_c7cb_4ded_a427_b32c_f9da_9341).as_bytes());
        entry.rich_metadata = crate::model::MetadataProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "properties": {"schema_version": 1, "image_width": 1920,
                    "image_height": 1080, "duration_ms": 65000,
                    "page_count": 4, "codec": "h264", "container": "mp4"}
            }))
            .unwrap(),
        );
        entry.share = crate::model::ShareProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "observed_at_ms": 10,
                "grants": [{
                    "grant_id": "c0d4e23f-c7cb-4ded-a427-b32cf9da9343",
                    "subject": {"kind": "bearer_link"},
                    "permission": "View",
                    "expires_at_ms": null,
                    "revoked": false,
                    "expired": false,
                    "used": false,
                    "exhausted": false,
                    "status": "active",
                    "redemptions": 0,
                    "max_redemptions": 1
                }]
            }))
            .unwrap(),
        );
        entry.history = crate::model::HistoryProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 1,
                "versions_truncated": false,
                "heads": ["a0d4e23f-c7cb-4ded-a427-b32cf9da9342"],
                "versions": [{
                    "id": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": null,
                    "metadata_hash": vec![3; 32],
                    "provenance": "Create"
                }],
                "conflicts_truncated": false,
                "conflicts": []
            }))
            .unwrap(),
        );
        let mut w = FileExplorerWidget::new();
        w.entries = vec![entry];
        w.selection.select_only(0);
        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Sharing & Access…" && *command == Some(FileCommand::ShowSharing)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Version History…" && *command == Some(FileCommand::ShowHistory)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Properties…" && *command == Some(FileCommand::ShowProperties)
        }));
        w.apply_command(FileCommand::ShowProperties);
        let panel = w.collab_panel.as_ref().unwrap();
        assert!(panel.title.contains("Properties"));
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("Collaborative record"))
        );
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("1 active grant"))
        );
        assert!(panel.lines.iter().any(|line| line.contains("1 version")));
        assert!(panel.lines.iter().any(|line| line.contains("1920 × 1080")));
        assert!(panel.lines.iter().any(|line| line.contains("1:05")));
        assert!(panel.lines.iter().any(|line| line.contains("Pages: 4")));
        w.apply_command(FileCommand::ShowSharing);
        let panel = w.collab_panel.as_ref().unwrap();
        assert!(panel.title.contains("Sharing"));
        assert!(panel.lines[0].contains("Anyone with the link"));
        w.apply_command(FileCommand::ShowHistory);
        assert!(w.collab_panel.is_none());
        assert!(w.history_registration().unwrap().is_some());
        w.apply_command(FileCommand::CloseCollabPanel);
        assert!(w.collab_panel.is_none());
    }

    #[test]
    fn history_activity_intent_preserves_selected_version_and_signed_identity() {
        let older = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let record = "b0d4e23f-c7cb-4ded-a427-b32cf9da9341";
        let mut entry = ent("report.txt", false, "text/plain");
        entry.history = crate::model::HistoryProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "record_id": record, "total_versions": 2,
                "versions_truncated": false, "heads": [current],
                "versions": [
                    {"id": older, "record_id": record, "parents": [],
                     "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                     "timestamp_ms": 9, "content_hash": vec![3; 32], "metadata_hash": vec![4; 32], "provenance": "Create"},
                    {"id": current, "record_id": record, "parents": [older],
                     "actor": {"person": vec![5; 32], "device": vec![6; 32]},
                     "timestamp_ms": 10, "content_hash": vec![7; 32], "metadata_hash": vec![8; 32], "provenance": "Edit"}
                ], "conflicts_truncated": false, "conflicts": []
            }))
            .unwrap(),
        );
        let mut w = FileExplorerWidget::new();
        w.entries = vec![entry];
        w.selection.select_only(0);
        w.apply_command(FileCommand::ShowHistory);
        let selected = collab_types::FileVersionId(uuid::Uuid::parse_str(older).unwrap());
        w.apply_command(FileCommand::ShowInWorkspaceActivity {
            record_id: record.into(),
            version_id: selected,
        });
        assert!(w.collab_panel.is_none());
        let Some(ui::widget::AppHostIntent::ShowWorkspaceActivity {
            resource_id,
            version_id,
            entry,
        }) = w.take_host_intent()
        else {
            panic!("Files hands the selected immutable version to Workspace Activity");
        };
        assert_eq!(resource_id, record);
        assert_eq!(version_id, Some(*selected.0.as_bytes()));
        let entry = entry.expect("the handoff carries a factual app-native event");
        assert_eq!(entry.id.as_bytes(), selected.0.as_bytes());
        assert_eq!(entry.context.unwrap().resource.unwrap().id, record);
    }

    // This scenario proves that governed presentation labels remain separate
    // from root-key command identity across the complete share-menu flow.
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end share flow must keep display identity and cryptographic command identity in one scenario"
    )]
    #[test]
    #[cfg(unix)]
    fn named_recipient_menu_uses_identity_label_but_commands_the_root_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        std::fs::write(&file, b"report").unwrap();
        xattr::set(
            dir.path(),
            crate::model::SHARE_CONTROL_XATTR,
            b"/run/tilo/app/tilo-fs-share.sock",
        )
        .unwrap();
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
        let person_bytes = root_key.verifying_key().to_bytes();
        let person = collab_types::Ed25519PublicKey::from_bytes(person_bytes);
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
        let device =
            collab_types::Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes());
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: person_bytes,
            device_public_key: device_key.verifying_key().to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root_key).unwrap();
        let mut w = FileExplorerWidget::new();
        w.workspace_roots = vec![dir.path().to_path_buf()];
        w.trusted_humans = crate::identity_directory::TrustedHumanDirectory::from_projection_v2(
            tilo_user_identity::TrustedIdentityProjectionV2 {
                schema_version: tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
                humans: vec![tilo_user_identity::GovernedHumanIdentityV2 {
                    root_public_key: person_bytes,
                    display_name: "Ada Lovelace".to_string(),
                    devices: vec![tilo_user_identity::GovernedDeviceIdentityV2 { certificate }],
                }],
            },
        )
        .unwrap();
        let mut entry = ent("report.txt", false, "text/plain");
        entry.path = file;
        entry.record_id = Some(*uuid::Uuid::from_u128(82).as_bytes());
        w.entries = vec![entry];
        w.selection.select_only(0);

        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Share with Ada Lovelace"
                && *command == Some(FileCommand::ShareWithPerson { person })
        }));
        assert!(spec.iter().any(|(label, command)| {
            label.contains("Ada Lovelace")
                && label.contains("Edit")
                && label.contains("no expiry")
                && *command
                    == Some(FileCommand::CreateShare(
                        crate::sharing::ShareComposerRequest {
                            subject: crate::sharing::ShareSubjectChoice::Named(person),
                            permission: collab_types::FilePermission::Edit,
                            expiry: crate::sharing::ShareExpiryChoice::Never,
                            link_uses: None,
                            recipient_devices: vec![device],
                        },
                    ))
        }));
        assert!(spec.iter().any(|(label, command)| {
            label.contains("Copy link")
                && label.contains("Edit")
                && label.contains("30 days")
                && label.contains("10 uses")
                && *command
                    == Some(FileCommand::CreateShare(
                        crate::sharing::ShareComposerRequest {
                            subject: crate::sharing::ShareSubjectChoice::Link,
                            permission: collab_types::FilePermission::Edit,
                            expiry: crate::sharing::ShareExpiryChoice::Month,
                            link_uses: Some(crate::sharing::LinkUseChoice::Bounded(10)),
                            recipient_devices: Vec::new(),
                        },
                    ))
        }));

        w.entries[0].share = Some(crate::model::ShareProjection {
            schema_version: 1,
            record_id: uuid::Uuid::from_u128(82).to_string(),
            observed_at_ms: 1,
            grants: vec![crate::model::ShareGrantProjection {
                grant_id: uuid::Uuid::from_u128(83).to_string(),
                subject: crate::model::ShareSubject::Person {
                    person: *person.as_bytes(),
                },
                permission: crate::model::FilePermission::Edit,
                expires_at_ms: None,
                revoked: false,
                expired: false,
                used: false,
                exhausted: false,
                status: crate::model::ShareGrantStatus::Active,
                redemptions: 0,
                max_redemptions: None,
            }],
        });
        w.apply_command(FileCommand::ShowSharing);
        let lines = &w.collab_panel.as_ref().unwrap().lines;
        assert!(lines.iter().any(|line| line.contains("Ada's laptop")));
        assert!(lines.iter().any(|line| line.contains("manages workspaces")));
        assert!(
            lines.iter().any(|line| line.contains("key ")),
            "the on-demand Sharing panel exposes certificate identity, not labels alone"
        );
    }

    #[test]
    fn running_files_reloads_new_renamed_removed_and_expired_governed_devices() {
        fn projection(
            root: &ed25519_dalek::SigningKey,
            name: &str,
            devices: &[(u8, &str, Option<u64>)],
        ) -> tilo_user_identity::TrustedIdentityProjectionV2 {
            let devices = devices
                .iter()
                .map(|(seed, label, expires_at_ns)| {
                    let device = ed25519_dalek::SigningKey::from_bytes(&[*seed; 32]);
                    let mut certificate = tilo_user_identity::DeviceCert {
                        user_public_key: root.verifying_key().to_bytes(),
                        device_public_key: device.verifying_key().to_bytes(),
                        device_label: (*label).into(),
                        capabilities: tilo_user_identity::DeviceCapabilities::member(),
                        issued_at_ns: 1,
                        expires_at_ns: *expires_at_ns,
                        signature: [0; 64],
                    };
                    tilo_user_identity::sign_device_cert(&mut certificate, root).unwrap();
                    tilo_user_identity::GovernedDeviceIdentityV2 { certificate }
                })
                .collect();
            tilo_user_identity::TrustedIdentityProjectionV2 {
                schema_version: tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
                humans: vec![tilo_user_identity::GovernedHumanIdentityV2 {
                    root_public_key: root.verifying_key().to_bytes(),
                    display_name: name.into(),
                    devices,
                }],
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted-identities.json");
        let root = ed25519_dalek::SigningKey::from_bytes(&[93; 32]);
        let person = collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes());
        let mut widget = FileExplorerWidget::new();

        std::fs::write(
            &path,
            serde_json::to_vec(&projection(&root, "Ada", &[(94, "Laptop", None)])).unwrap(),
        )
        .unwrap();
        assert!(widget.refresh_trusted_identities(Some(&path)));
        assert_eq!(
            widget.trusted_humans.human(&person).unwrap().devices.len(),
            1
        );

        std::fs::write(
            &path,
            serde_json::to_vec(&projection(
                &root,
                "Ada Renamed",
                &[(94, "Laptop", None), (95, "Phone", None)],
            ))
            .unwrap(),
        )
        .unwrap();
        assert!(widget.refresh_trusted_identities(Some(&path)));
        let human = widget.trusted_humans.human(&person).unwrap();
        assert_eq!(human.display_name, "Ada Renamed");
        assert_eq!(human.devices.len(), 2);

        std::fs::write(
            &path,
            serde_json::to_vec(&projection(&root, "Ada Renamed", &[(95, "Phone", Some(2))]))
                .unwrap(),
        )
        .unwrap();
        assert!(widget.refresh_trusted_identities(Some(&path)));
        assert!(
            widget
                .trusted_humans
                .human(&person)
                .unwrap()
                .devices
                .is_empty(),
            "removed and expired devices cannot remain usable after reload"
        );

        std::fs::write(
            &path,
            serde_json::to_vec(&tilo_user_identity::TrustedIdentityProjectionV2::empty()).unwrap(),
        )
        .unwrap();
        assert!(widget.refresh_trusted_identities(Some(&path)));
        assert!(widget.trusted_humans.humans().is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn collaborative_search_activation_resolves_the_latest_path_by_record_id() {
        use ui::widget::Widget;

        let dir = tempfile::tempdir().unwrap();
        let record_id = uuid::Uuid::from_u128(0xabc);
        let nested = dir.path().join("nested");
        let file = nested.join("renamed.txt");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&file, b"hello").unwrap();
        xattr::set(&file, crate::model::RECORD_ID_XATTR, record_id.as_bytes()).unwrap();
        let projection = serde_json::json!({
            "schema_version": 1,
            "truncated": false,
            "records": [{
                "record_id": record_id,
                "path": "/nested/renamed.txt",
                "is_dir": false
            }]
        });
        xattr::set(
            dir.path(),
            crate::model::RECORD_INDEX_XATTR,
            &serde_json::to_vec(&projection).unwrap(),
        )
        .unwrap();
        let mut widget = FileExplorerWidget::new();
        widget.current_path = dir.path().to_path_buf();
        widget.entries.clear();
        widget.workspace_roots = vec![dir.path().to_path_buf()];
        widget.collaborative_record_pager = RefCell::new(
            crate::model::WorkspaceRecordIndexPager::new(&widget.workspace_roots),
        );
        // Workspace roots are an explicit external-source revision boundary;
        // refresh once, then Ctrl+K itself performs no mount traversal.
        widget.refresh_external_search_catalogs();

        let providers = widget.search_providers();
        // Places and remote-account discovery remain separate providers; find
        // the collaborative hit by its stable record action rather than by a
        // brittle provider position or a developer's local account settings.
        let hit = providers
            .iter()
            .flat_map(|provider| provider.search("renamed"))
            .find(|result| {
                result.action_address()
                    == Some(crate::search_provider::record_reveal_address(record_id))
            })
            .expect("collaborative record is searchable");
        let doc = automerge::AutoCommit::new();
        let address = hit.action_address().expect("canonical reveal action");
        let mut resources = Widget::resources(&widget, &doc);
        resources.push(
            Widget::resource(&widget, &doc, &address.resource)
                .expect("late record materializes exactly for this address"),
        );
        let spec = ui::tilo_actions::door::resolve(&resources, &address)
            .expect("search reveal action is exported");
        let kind = spec.kind;
        let encoded =
            ui::tilo_actions::door::encode_json(&resources, &address, &serde_json::Map::new())
                .expect("reveal action encodes");
        assert!(
            Widget::invoke_resource_operation(
                &mut widget,
                kind,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &automerge::ROOT,
            )
            .unwrap()
        );
        assert_eq!(widget.current_path, nested);
        assert_eq!(widget.selection.primary(), Some(0));
        assert_eq!(widget.entries[0].path, file);
    }

    #[test]
    #[cfg(unix)]
    fn share_composer_authority_round_trip_never_blocks_the_ui_command() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        let socket = dir.path().join("share.sock");
        std::fs::write(&file, b"report").unwrap();
        xattr::set(
            dir.path(),
            crate::model::SHARE_CONTROL_XATTR,
            socket.as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).unwrap();
            let mut request = vec![0; u32::from_be_bytes(length) as usize];
            stream.read_exact(&mut request).unwrap();
            seen_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let response = collab_types::ShareControlResponseV1 {
                protocol_version: collab_types::SHARE_CONTROL_VERSION,
                result: collab_types::ShareControlResultV1::BearerCreatedV2 {
                    uri: "tilo-file://bounded".into(),
                },
            };
            let response = collab_types::canonical_bytes(&response).unwrap();
            stream
                .write_all(&(response.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&response).unwrap();
        });

        let mut widget = FileExplorerWidget::new();
        widget.workspace_roots = vec![dir.path().to_path_buf()];
        let mut entry = ent("report.txt", false, "text/plain");
        entry.path = file;
        entry.record_id = Some(*uuid::Uuid::from_u128(0xdef).as_bytes());
        widget.entries = vec![entry];
        widget.selection.select_only(0);
        assert!(widget.apply_command(FileCommand::CreateShare(
            crate::sharing::ShareComposerRequest {
                subject: crate::sharing::ShareSubjectChoice::Link,
                permission: collab_types::FilePermission::Edit,
                expiry: crate::sharing::ShareExpiryChoice::Month,
                link_uses: Some(crate::sharing::LinkUseChoice::Bounded(10)),
                recipient_devices: Vec::new(),
            },
        )));
        // The daemon has the request but deliberately cannot reply yet. Reaching
        // this receive proves the UI command returned independently of I/O.
        seen_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(widget.share_pending_path.is_some());
        release_tx.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn named_share_status_distinguishes_pending_from_delivered() {
        let pending = t_args(
            named_delivery_status_key(collab_types::NamedShareDeliveryStateV1::Pending),
            &[arg("person", "Ada")],
        );
        let delivered = t_args(
            named_delivery_status_key(collab_types::NamedShareDeliveryStateV1::Delivered),
            &[arg("person", "Ada")],
        );
        assert!(pending.contains("pending"));
        assert!(delivered.contains("completed"));
        assert_ne!(pending, delivered);
    }

    #[test]
    fn previous_immutable_version_is_an_explicit_restore_command() {
        let previous = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let mut entry = ent("history.txt", false, "text/plain");
        entry.history = crate::model::HistoryProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 2,
                "versions_truncated": false,
                "heads": [current],
                "versions": [{
                    "id": previous,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": vec![3; 32],
                    "metadata_hash": vec![4; 32],
                    "provenance": "Create"
                }, {
                    "id": current,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [previous],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 10,
                    "content_hash": vec![5; 32],
                    "metadata_hash": vec![6; 32],
                    "provenance": "Edit"
                }],
                "conflicts_truncated": false,
                "conflicts": []
            }))
            .unwrap(),
        );
        let mut widget = FileExplorerWidget::new();
        widget.entries = vec![entry];
        widget.selection.select_only(0);
        let expected = collab_types::FileVersionId(uuid::Uuid::parse_str(previous).unwrap());
        assert!(
            widget
                .context_menu_spec(&[])
                .iter()
                .any(|(label, command)| {
                    label == "Restore Previous Version"
                        && *command
                            == Some(FileCommand::RestoreVersion {
                                version_id: expected,
                            })
                })
        );
    }

    #[test]
    fn open_text_conflict_menu_requires_review_before_an_exact_merge() {
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let other = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let conflict = uuid::Uuid::parse_str("c0d4e23f-c7cb-4ded-a427-b32cf9da9343").unwrap();
        let mut entry = ent("conflicted.qdoc", false, "application/x-qdoc");
        entry.content_hash = Some([5; 32]);
        entry.history = crate::model::HistoryProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 2,
                "versions_truncated": false,
                "heads": [current, other],
                "versions": [{
                    "id": other,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": vec![3; 32],
                    "metadata_hash": vec![4; 32],
                    "provenance": "Edit"
                }, {
                    "id": current,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [other],
                    "actor": {"person": vec![6; 32], "device": vec![7; 32]},
                    "timestamp_ms": 10,
                    "content_hash": vec![5; 32],
                    "metadata_hash": vec![8; 32],
                    "provenance": "Edit"
                }],
                "conflicts_truncated": false,
                "conflicts": [{
                    "id": conflict,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "versions": [other, current],
                    "status": "Open",
                    "resolution": null
                }]
            }))
            .unwrap(),
        );
        let mut widget = FileExplorerWidget::new();
        widget.entries = vec![entry];
        widget.selection.select_only(0);
        let menu = widget.context_menu_spec(&[]);
        let current_id = collab_types::FileVersionId(uuid::Uuid::parse_str(current).unwrap());
        let other_id = collab_types::FileVersionId(uuid::Uuid::parse_str(other).unwrap());
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: collab_types::ConflictStrategy::KeepCurrent,
                    version_id: current_id,
                })
        }));
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: collab_types::ConflictStrategy::UseOther,
                    version_id: other_id,
                })
        }));
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: collab_types::ConflictStrategy::KeepBoth,
                    version_id: other_id,
                })
        }));
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ReviewConflictMerge {
                    conflict_id: conflict,
                    version_id: other_id,
                    other_device: [2; 32],
                })
        }));
    }

    #[test]
    fn merge_review_only_enables_apply_after_a_safe_daemon_preflight() {
        let conflict_id = uuid::Uuid::from_u128(42);
        let version_id = collab_types::FileVersionId(uuid::Uuid::from_u128(43));
        let mut widget = FileExplorerWidget::new();
        widget.show_collab_panel(CollabPanel {
            history: None,
            title: "Review merge".into(),
            summary: "Checking".into(),
            lines: Vec::new(),
            action: None,
        });
        *widget
            .merge_preflight_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
            conflict_id,
            version_id,
            Ok(collab_types::MergePreflightProjectionV1 {
                schema_version: collab_types::MERGE_PREFLIGHT_VERSION,
                conflict_id,
                safe: true,
                explanation: "Changes are disjoint.".into(),
                diff_summary: "Both revisions are included.".into(),
            }),
        ));
        widget.merge_preflight_pending = Some((conflict_id, version_id));
        assert!(widget.poll_merge_preflight());
        assert!(matches!(
            widget.collab_panel.as_ref().and_then(|panel| panel.action.as_ref()),
            Some(CollabPanelAction::ApplySafeMerge {
                conflict_id: actual_conflict,
                version_id: actual_version,
            }) if *actual_conflict == conflict_id && *actual_version == version_id
        ));

        *widget
            .merge_preflight_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
            conflict_id,
            version_id,
            Ok(collab_types::MergePreflightProjectionV1 {
                schema_version: collab_types::MERGE_PREFLIGHT_VERSION,
                conflict_id,
                safe: false,
                explanation: "Both revisions changed the same text region.".into(),
                diff_summary: "No proposed content was produced.".into(),
            }),
        ));
        widget.merge_preflight_pending = Some((conflict_id, version_id));
        assert!(widget.poll_merge_preflight());
        let panel = widget.collab_panel.as_ref().unwrap();
        assert!(
            panel.action.is_none(),
            "conflicting edits must not expose Apply safe merge"
        );
        assert!(panel.summary.contains("Needs review"));
    }

    #[test]
    fn late_merge_preflight_cannot_repurpose_an_unrelated_confirmation_sheet() {
        let stale_conflict = uuid::Uuid::from_u128(50);
        let current_conflict = uuid::Uuid::from_u128(51);
        let version_id = collab_types::FileVersionId(uuid::Uuid::from_u128(52));
        let mut widget = FileExplorerWidget::new();
        widget.show_collab_panel(CollabPanel {
            history: None,
            title: "Delete permanently?".into(),
            summary: "This cannot be undone".into(),
            lines: Vec::new(),
            action: Some(CollabPanelAction::EmptyTrash),
        });
        widget.merge_preflight_pending = Some((current_conflict, version_id));
        *widget
            .merge_preflight_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
            stale_conflict,
            version_id,
            Ok(collab_types::MergePreflightProjectionV1 {
                schema_version: collab_types::MERGE_PREFLIGHT_VERSION,
                conflict_id: stale_conflict,
                safe: true,
                explanation: "Changes are disjoint.".into(),
                diff_summary: "Both revisions are included.".into(),
            }),
        ));

        assert!(!widget.poll_merge_preflight());
        let panel = widget.collab_panel.as_ref().unwrap();
        assert_eq!(panel.title, "Delete permanently?");
        assert!(matches!(panel.action, Some(CollabPanelAction::EmptyTrash)));
    }

    #[test]
    #[cfg(unix)]
    fn owner_context_menu_revokes_each_exact_active_grant() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("shared.txt");
        std::fs::write(&file, b"shared").unwrap();
        let socket = root.path().join("share-control.sock");
        xattr::set(
            root.path(),
            crate::model::SHARE_CONTROL_XATTR,
            socket.as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        let grant = uuid::Uuid::parse_str("c0d4e23f-c7cb-4ded-a427-b32cf9da9343").unwrap();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = file;
        entry.share = crate::model::ShareProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "observed_at_ms": 10,
                "grants": [{
                    "grant_id": grant.to_string(),
                    "subject": {"kind": "bearer_link"},
                    "permission": "View",
                    "expires_at_ms": null,
                    "revoked": false,
                    "expired": false,
                    "used": false,
                    "exhausted": false,
                    "status": "active",
                    "redemptions": 0,
                    "max_redemptions": 2
                }]
            }))
            .unwrap(),
        );
        let mut w = FileExplorerWidget::new();
        w.workspace_roots = vec![root.path().to_path_buf()];
        w.entries = vec![entry];
        w.selection.select_only(0);
        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Revoke shared link"
                && *command == Some(FileCommand::RevokeShare { grant_id: grant })
        }));
    }

    #[test]
    fn archives_browse_as_read_only_folders_and_extract_from_inside() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(source.join("Photos")).unwrap();
        std::fs::write(source.join("Photos/one.jpg"), b"one").unwrap();
        std::fs::write(source.join("readme.txt"), b"read me").unwrap();
        let archive = temp.path().join("Holiday.zip");
        crate::archive::create_zip(
            &[source.join("Photos"), source.join("readme.txt")],
            &archive,
        )
        .unwrap();

        let mut files = FileExplorerWidget::with_persistence(None, Some(temp.path().into()));
        files.navigate(archive);
        assert_eq!(files.entries_snapshot().len(), 2);
        assert!(
            files
                .entries_snapshot()
                .iter()
                .any(|entry| entry.name == "Photos" && entry.is_dir)
        );
        assert!(
            files
                .entries_snapshot()
                .iter()
                .any(|entry| entry.name == "readme.txt" && !entry.is_dir)
        );

        let photos = files
            .entries_snapshot()
            .iter()
            .position(|entry| entry.name == "Photos")
            .unwrap();
        files.open_entry(photos, &mut SystemRunner).unwrap();
        assert_eq!(files.entries_snapshot().len(), 1);
        assert_eq!(files.entries_snapshot()[0].name, "one.jpg");

        files.open_view_menu();
        let menu_labels: Vec<_> = files
            .context_menu
            .items()
            .iter()
            .filter_map(|item| item.label())
            .collect();
        assert!(menu_labels.contains(&"Extract archive…"));
        assert!(menu_labels.iter().all(|label| !label.starts_with('✓')));
        assert!(!menu_labels.contains(&"New Folder"));
        assert!(!menu_labels.contains(&"Open Terminal Here"));
        let list_choice = files
            .context_menu
            .items()
            .iter()
            .find(|item| item.label() == Some("List"))
            .unwrap();
        assert_eq!(list_choice.checked(), Some(true));

        files.selection.select_only(0);
        assert!(files.apply_command(FileCommand::Delete));
        assert!(source.join("Photos/one.jpg").exists());
        let read_only = t("files-archive-read-only");
        assert_eq!(files.status_line(), Some(read_only.as_str()));

        assert!(files.apply_command(FileCommand::ExtractCurrentArchive));
        assert_eq!(
            std::fs::read(temp.path().join("Holiday/Photos/one.jpg")).unwrap(),
            b"one"
        );
        assert_eq!(
            std::fs::read(temp.path().join("Holiday/readme.txt")).unwrap(),
            b"read me"
        );
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod responsive_body_tests {
    //! The places list is a *navigation* sidebar over the file view, on the
    //! shared `NavShell`: a comfortably-wide window docks it beside the files;
    //! a narrow window turns it into an overlay drawer over them. The file
    //! list is the content at every width — it is never pushed off the window
    //! to make room for the list of places. Drives the real production path
    //! (`build_layout` → `apply_layout` at a size, then `handle_event`).
    use super::*;
    use std::collections::HashMap;
    use taffy::prelude::*;
    use ui::theme::Theme;

    include!("../native_proof_tests.rs");

    struct H {
        w: FileExplorerWidget,
        taffy: TaffyTree<()>,
        node: NodeId,
        state: ui::State,
        tr: ui::text_render::TextRenderer,
    }

    impl H {
        fn new(dir: Option<&std::path::Path>, width: f32, height: f32) -> Self {
            let mut w = FileExplorerWidget::with_persistence(None, None);
            if let Some(d) = dir {
                w.navigate(d.to_path_buf());
            }
            Self::from_widget(w, width, height)
        }

        /// Lay out a pre-built widget (e.g. one with persisted favourites, which
        /// `new`'s home-less widget lacks).
        fn from_widget(w: FileExplorerWidget, width: f32, height: f32) -> Self {
            let mut h = H {
                w,
                taffy: TaffyTree::new(),
                node: NodeId::new(0),
                state: ui::State::new(),
                tr: ui::text_render::TextRenderer::new(),
            };
            h.lay_out(width, height);
            // Match the live host: a responsive child discovers its width
            // during the first layout and asks for one immediate structural
            // reflow when it crosses a breakpoint.
            if h.w.take_structure_dirty() {
                h.lay_out(width, height);
            }
            h
        }

        fn lay_out(&mut self, width: f32, height: f32) {
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            self.w.sync(&mut self.state.doc, &self.state.root);
            let node = self.w.build_layout(&mut taffy, &theme, &mut nm);
            let measure_map = nm.clone();
            let trm = &mut self.tr;
            taffy
                .compute_layout_with_measure(
                    node,
                    Size {
                        width: AvailableSpace::Definite(width),
                        height: AvailableSpace::Definite(height),
                    },
                    |k, a, id, _, _| {
                        if let Some(&p) = measure_map.get(&id) {
                            unsafe { (*p).measure(k, a, trm) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            self.w.apply_layout(&taffy, node, Point::ORIGIN);
            self.taffy = taffy;
            self.node = node;
        }

        fn dispatch(&mut self, ev: &winit::event::WindowEvent) {
            let mut handled = false;
            let mut focus = None;
            self.w.handle_event(
                ev,
                1.0,
                &winit::event::Modifiers::default(),
                &mut self.state.doc,
                &self.state.root,
                &mut handled,
                &mut focus,
                self.node,
                &self.taffy,
            );
        }

        fn cursor(&mut self, x: f64, y: f64) {
            self.dispatch(&winit::event::WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(x, y),
            });
        }

        fn right_press(&mut self) {
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Right,
            });
        }

        fn left_release(&mut self) {
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            });
        }

        fn left_press(&mut self) {
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            });
        }

        /// The labels of the currently-open context/favourite menu, if any.
        fn open_menu_labels(&self) -> Vec<String> {
            if !self.w.context_menu.is_open() {
                return Vec::new();
            }
            self.w
                .context_menu
                .items()
                .iter()
                .filter_map(|i| i.label().map(str::to_string))
                .collect()
        }

        /// The laid-out rect of a named widget in the live tree. Asserting on
        /// the *file view's* own rect is what makes the narrow test about the
        /// files rather than about whichever pane happens to be full-width.
        fn rect_of(&self, name: &str) -> Option<vello::kurbo::Rect> {
            fn find(w: &dyn Widget, name: &str) -> Option<vello::kurbo::Rect> {
                if w.name() == name {
                    return Some(w.get_rect());
                }
                for c in w.children() {
                    if let Some(r) = find(c, name) {
                        return Some(r);
                    }
                }
                None
            }
            find(&self.w, name)
        }

        /// Run a real draw pass so immediate-mode children (the file view)
        /// publish their per-frame geometry — e.g. the in-place rename field's
        /// rect.
        fn draw(&mut self) {
            let theme = Theme::dark();
            let presence = ui::presence::PresenceCtx::empty();
            let permissions = ui::permissions::PermissionContext::permissive();
            let mut scene = vello::Scene::new();
            self.w.draw_with_presence(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut self.tr,
                &theme,
                &presence,
                &permissions,
            );
        }

        fn press_named(&mut self, key: winit::keyboard::NamedKey) {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = winit::keyboard::Key::Named(key);
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = winit::event::ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            self.dispatch(&winit::event::WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            });
        }

        fn type_char(&mut self, s: &str) {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = winit::keyboard::Key::Character(winit::keyboard::SmolStr::new(s));
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = winit::event::ElementState::Pressed;
            ev.repeat = false;
            ev.text = Some(winit::keyboard::SmolStr::new(s));
            self.dispatch(&winit::event::WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            });
        }
    }

    fn first_scrollable_viewport(
        widget: &mut dyn Widget,
    ) -> Option<(vello::kurbo::Rect, vello::kurbo::Rect)> {
        if let Some(scrollable) = widget
            .as_any_mut()
            .and_then(|value| value.downcast_mut::<Scrollable>())
        {
            return Some((scrollable.get_rect(), scrollable.content_viewport_rect()));
        }
        for child in widget.children_mut() {
            if let Some(viewport) = first_scrollable_viewport(child) {
                return Some(viewport);
            }
        }
        None
    }

    #[test]
    fn body_docks_wide_and_reflows_to_push_when_narrowed() {
        let mut h = H::new(None, 1200.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            ShellMode::Wide,
            "a wide window docks the places list beside the file view"
        );
        // Resize DOWN on the same instance (a real window resize re-runs
        // apply_layout) — the body must reflow to a single pane, not freeze.
        h.lay_out(520.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            ShellMode::Narrow,
            "a narrow window collapses the body to a single pane"
        );
        // ...and back up again proves it isn't one-way.
        h.lay_out(1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
    }

    #[test]
    fn toolbar_reprioritizes_before_desktop_controls_can_overlap_on_phone_width() {
        let mut h = H::new(None, 390.0, 760.0);
        assert!(
            h.w.toolbar.is_compact(),
            "the first narrow frame keeps the path and navigation clear"
        );
        h.w.forward.push(PathBuf::from("/later"));
        h.lay_out(390.0, 760.0);
        h.w.open_view_menu();
        assert!(
            h.open_menu_labels().iter().any(|label| label == "Forward"),
            "compact overflow retains history navigation rather than removing it"
        );

        h.lay_out(560.0, 760.0);
        assert!(
            h.w.toolbar.is_compact(),
            "the 560 px tablet width keeps the location readable instead of overlapping it with desktop controls"
        );

        // The width is discovered during layout; one follow-up composition is
        // how the live app promotes wide controls after a real resize.
        h.lay_out(1200.0, 720.0);
        assert!(
            h.w.take_structure_dirty(),
            "crossing the breakpoint requests a structural toolbar rebuild"
        );
        h.lay_out(1200.0, 720.0);
        assert!(
            !h.w.toolbar.is_compact(),
            "wide windows restore direct view and sort controls"
        );
    }

    #[test]
    fn toolbar_more_menu_tracks_its_trigger_and_flips_inside_the_viewport() {
        let mut h = H::new(None, 900.0, 720.0);
        h.w.open_view_menu();
        let trigger =
            h.w.toolbar
                .view_menu_rect()
                .expect("More trigger is laid out");
        let menu = h.w.context_menu.get_rect();
        assert!(
            (menu.x1 - trigger.x1).abs() <= 1.0 && menu.y0 >= trigger.y1,
            "toolbar menu must sit directly below and right-aligned to More: trigger={trigger:?} menu={menu:?}"
        );

        h.w.open_menu_right_aligned(
            vec![
                ("One".into(), None, None),
                ("Two".into(), None, None),
                ("Three".into(), None, None),
                ("Four".into(), None, None),
            ],
            Point::new(890.0, 710.0),
            680.0,
        );
        let flipped = h.w.context_menu.get_rect();
        assert!(
            flipped.x0 >= h.w.rect.x0
                && flipped.x1 <= h.w.rect.x1
                && flipped.y0 >= h.w.rect.y0
                && flipped.y1 <= 681.0,
            "a constrained menu must flip above its trigger and remain on-screen: {flipped:?}"
        );
    }

    #[test]
    fn narrow_error_retry_uses_the_shared_button_and_recovers() {
        let mut h = H::new(None, 390.0, 780.0);
        h.w.debug_show_error_state();
        h.lay_out(390.0, 780.0);
        h.draw();

        let retry = h.w.state_retry.get_rect();
        assert!(
            retry.width() >= 120.0 && retry.y0 > 0.0,
            "retry is a usable target: {retry:?}"
        );
        h.cursor(retry.center().x, retry.center().y);
        h.left_press();
        h.left_release();
        let intent = Widget::take_control_invocation(&mut h.w)
            .expect("the shared button emits a resource action");
        assert_eq!(intent.address.resource, "tilo.control:files.retry");
        assert_eq!(intent.address.action, "invoke");
        assert!(
            h.w.invoke_resource_operation(
                tilo_actions::OperationKind::Effect,
                "tilo.ui.control.invoke",
                br#""files.retry""#,
                &h.state.doc,
                &h.state.root,
            )
            .unwrap(),
            "the resource action is accepted by the Files action boundary"
        );
        assert!(
            h.w.error().is_none(),
            "the shared button dispatches the normal refresh command"
        );
    }

    #[test]
    fn shared_toolbar_resource_action_opens_view_options() {
        let mut h = H::new(None, 1200.0, 720.0);
        assert!(
            h.w.invoke_resource_operation(
                tilo_actions::OperationKind::Effect,
                "tilo.ui.control.invoke",
                br#""files.view_menu""#,
                &h.state.doc,
                &h.state.root,
            )
            .unwrap(),
            "the same resource action an AT-SPI client discovers is handled"
        );
        assert!(
            h.w.context_menu.is_open(),
            "resource/AT-SPI activation opens the real View options menu"
        );
        assert!(
            h.w.menu_commands
                .iter()
                .any(|command| { matches!(command, Some(FileCommand::NewFolder)) }),
            "the real menu owns the New folder follow-up; this is not a hidden state toggle"
        );
    }

    #[test]
    fn accesskit_view_options_activation_updates_the_exported_menu_tree() {
        // This is the same retained-control path used by the Unix AT-SPI
        // provider: find the visual button in an AccessKit update, dispatch
        // its action through App, then require the newly opened menu item in
        // the next update. A `DoAction` reply alone is not enough.
        let mut app = ui::App::new(
            ui::State::new(),
            Box::new(FileExplorerWidget::with_persistence(None, None)),
        );
        app.init_layout();
        let before = app.build_a11y_tree_update(1.0);
        let view_options = before
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == ui::accesskit::Role::Button && node.label() == Some("View options")
            })
            .map(|(id, _)| *id)
            .expect("the retained View options button is exported");

        assert_eq!(
            app.dispatch_a11y_action(view_options, &ui::a11y::Action::Activate),
            None
        );
        let after = app.build_a11y_tree_update(1.0);
        assert!(
            after
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("New folder")),
            "AT-SPI activation must publish the actual View options menu, not only acknowledge Click"
        );
    }

    #[test]
    fn archive_extraction_stays_directly_reachable_on_phone_and_desktop() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("note.txt");
        std::fs::write(&source, b"hello").unwrap();
        let archive = temp.path().join("notes.zip");
        crate::archive::create_zip(&[source], &archive).unwrap();

        let mut h = H::new(Some(&archive), 390.0, 760.0);
        let extract = t("files-archive-extract");
        let has_extract = |toolbar: &crate::toolbar::FileToolbar| {
            toolbar.children().into_iter().any(|child| {
                child
                    .accessibility()
                    .is_some_and(|node| node.name.as_deref() == Some(extract.as_str()))
            })
        };
        assert!(has_extract(&h.w.toolbar), "phone toolbar exposes Extract");
        assert!(
            h.w.toolbar.view_menu_rect().is_none(),
            "archive toolbar must not expose a one-item duplicate menu"
        );
        assert_eq!(h.w.footer_label(), t("files-archive-read-only"));

        h.lay_out(1200.0, 760.0);
        if h.w.take_structure_dirty() {
            h.lay_out(1200.0, 760.0);
        }
        assert!(has_extract(&h.w.toolbar), "desktop toolbar exposes Extract");
        assert!(h.w.toolbar.view_menu_rect().is_none());
    }

    /// The regression: at ~600 px the window showed the breadcrumb and the
    /// FAVOURITES / SYSTEM list and **no files at all**, while the footer
    /// underneath read "27 items". The places list had been declared a content
    /// overview, so the narrow layout promoted it to the full-screen page and
    /// pushed the file view off the window.
    #[test]
    fn narrow_shows_the_files_not_the_places_list() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
        }
        let h = H::new(Some(dir.path()), 600.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Narrow);

        assert!(
            !h.w.debug_places_open(),
            "the places drawer starts closed, so the files are what you see",
        );
        // The file view's OWN rect, not "whichever pane is full-width": under
        // the original bug the places list was the full-width pane and the file
        // view was laid out off the right edge of the window.
        let files = h
            .rect_of("FileView")
            .expect("the file view must be in the tree at a narrow width");
        assert!(
            files.width() > 0.0 && files.height() > 0.0,
            "the file view must be laid out at a real size; got {files:?}",
        );
        assert!(
            files.x0 >= 0.0 && files.x1 <= 600.0,
            "and inside the window rather than pushed off it: {files:?}",
        );
        assert!(
            (h.w.debug_content_rect().width() - 600.0).abs() <= f64::EPSILON,
            "the content pane spans the whole narrow window",
        );
    }

    #[test]
    fn floating_status_bubble_reserves_a_noninteractive_bottom_lane() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..40 {
            std::fs::write(dir.path().join(format!("item-{index}.txt")), b"x").unwrap();
        }
        let mut h = H::new(Some(dir.path()), 390.0, 720.0);
        let (scrollable, content_viewport) = first_scrollable_viewport(&mut h.w)
            .expect("Files content is wrapped in the shared scrollable");

        assert!(
            scrollable.y1 - content_viewport.y1
                >= f64::from(STATUS_BUBBLE_SAFE_INSET_PX) - f64::EPSILON,
            "the item/status pill has a dedicated bottom lane rather than overlaying files: \
             scrollable={scrollable:?} viewport={content_viewport:?}"
        );
    }

    #[test]
    fn keyboard_context_menu_reaches_selection_actions_and_executes_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.txt"), b"draft").unwrap();
        let mut h = H::new(Some(dir.path()), 900.0, 720.0);

        // Arrow navigation selects the first row. The platform Menu key must
        // open the same selection-aware surface as right-click.
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
        h.press_named(winit::keyboard::NamedKey::ContextMenu);
        assert!(h.w.context_menu.is_open());
        let labels = h.open_menu_labels();
        assert!(labels.iter().any(|label| label == "Open"));
        assert!(labels.iter().any(|label| label == "Rename"));

        // Menu keyboard handling skips separators and maps the selected row
        // back onto the exact FileCommand. Down selects Open, Down selects
        // Rename, and Enter executes it without any pointer event.
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
        h.press_named(winit::keyboard::NamedKey::Enter);
        assert!(!h.w.context_menu.is_open());
        assert!(h.w.renaming.is_some(), "keyboard menu executes Rename");
    }

    /// The drawer draws *over* the files rather than displacing them, and the
    /// files stay full-width behind it (HIG §1a).
    #[test]
    fn narrow_places_drawer_overlays_the_files_without_displacing_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 600.0, 720.0);
        let before = h.w.debug_content_rect();

        h.w.debug_open_places_drawer();
        h.lay_out(600.0, 720.0);
        h.w.debug_open_places_drawer();
        assert!(h.w.debug_places_open(), "the drawer is revealed");
        assert_eq!(
            h.w.debug_content_rect(),
            before,
            "the file view must not move or shrink when the drawer opens",
        );
        assert!(
            h.w.status_bubble_is_occluded_by_places_drawer(),
            "the item/status bubble belongs behind the modal drawer scrim"
        );
    }

    #[test]
    fn narrow_places_drawer_scrim_is_a_modal_dismiss_backdrop_not_file_input() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("behind-the-drawer.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 600.0, 720.0);
        h.w.debug_open_places_drawer();
        h.lay_out(600.0, 720.0);
        h.w.debug_open_places_drawer();
        let sidebar = h.w.debug_sidebar_rect();
        let scrim_x = sidebar.x1 + 24.0;
        let scrim_y = h.w.debug_content_rect().y0 + 180.0;

        // Right-clicks over the uncovered strip used to be able to create a
        // file-view context menu. They now remain in the modal backdrop.
        h.cursor(scrim_x, scrim_y);
        h.right_press();
        assert!(h.w.debug_places_open());
        assert!(
            !h.w.context_menu.is_open() && h.w.selected_indices().is_empty(),
            "the backdrop cannot select or open a menu for the file behind it"
        );

        // A primary press is the intentional dismissal affordance, and still
        // must not reach the underlying list/scrollbar on that same event.
        h.left_press();
        assert!(!h.w.debug_places_open());
        assert!(h.w.selected_indices().is_empty());

        h.w.debug_open_places_drawer();
        let semantic_children = h.w.nav.children();
        assert_eq!(semantic_children.len(), 1);
        assert_eq!(semantic_children[0].name(), "PlacesSidebar");
    }

    /// Picking a destination in the narrow drawer dismisses it, leaving the
    /// user on the files they navigated to.
    #[test]
    fn narrow_navigation_dismisses_the_drawer_and_leaves_the_files_showing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut h = H::new(Some(dir.path()), 520.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Narrow);
        h.w.debug_open_places_drawer();
        assert!(h.w.debug_places_open());

        // A place-navigation (what tapping a place in the list emits). Inject
        // the command the places list pushes, then pump an event so
        // `handle_event` drains it and applies the dismissal wiring.
        crate::command::push(&h.w.cmds, FileCommand::Navigate(dir.path().join("sub")));
        h.cursor(10.0, 10.0);
        assert_eq!(
            h.w.current_path(),
            dir.path().join("sub"),
            "the navigation applied"
        );
        assert!(
            !h.w.debug_places_open(),
            "navigating dismisses the drawer so the files are what remains",
        );
        assert!(
            (h.w.debug_content_rect().width() - 520.0).abs() <= f64::EPSILON,
            "and the file view still owns the whole window",
        );
    }

    /// An inline rename is edited *in place* — a field placed over the row's
    /// name inside the file view — not in a bottom bar. The file view publishes
    /// the field rect during paint; it must sit within the file rows.
    #[test]
    fn rename_is_edited_in_place_over_the_row() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
        let idx =
            h.w.displayed_entries()
                .iter()
                .position(|e| e.name == "old.txt")
                .unwrap();
        h.w.select(Some(idx));
        assert!(h.w.apply_command(FileCommand::Rename));
        assert!(h.w.is_renaming());

        // Rebuild with the rename active, then paint so the file view publishes
        // the editor's on-screen rect.
        h.lay_out(1000.0, 720.0);
        h.draw();

        let field =
            h.w.rename_rect
                .get()
                .expect("the in-place editor is placed over the row");
        let files = h.rect_of("FileView").expect("the file view is present");
        assert!(
            field.y0 >= files.y0 - 1.0 && field.y1 <= files.y1 + 1.0,
            "the rename field sits within the file rows (in place), not a bottom bar: \
             field={field:?} files={files:?}",
        );
        assert!(
            field.width() > 40.0,
            "the field spans the name column: {field:?}"
        );

        // Cancelling clears the published rect (nothing left drawn).
        h.w.cancel_rename();
        h.lay_out(1000.0, 720.0);
        h.draw();
        assert!(
            h.w.rename_rect.get().is_none(),
            "cancel removes the in-place editor"
        );
    }

    /// Typing into the in-place editor and pressing Enter renames the file on
    /// disk — driving the real keystroke path through the app into the shared
    /// `TextInput` and its CRDT-backed field. F2 opens the editor.
    #[test]
    fn typing_in_the_in_place_editor_renames_on_enter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
        let idx =
            h.w.displayed_entries()
                .iter()
                .position(|e| e.name == "a.txt")
                .unwrap();
        h.w.select(Some(idx));

        // F2 begins the rename; rebuild + paint places + seeds the editor.
        h.press_named(winit::keyboard::NamedKey::F2);
        assert!(h.w.is_renaming(), "F2 starts an inline rename");
        h.lay_out(1000.0, 720.0);
        h.draw();

        // The seed selected the whole name, so typing replaces it.
        for ch in ["b", ".", "m", "d"] {
            h.type_char(ch);
        }
        h.press_named(winit::keyboard::NamedKey::Enter);

        assert!(!h.w.is_renaming(), "Enter commits and closes the editor");
        assert!(
            dir.path().join("b.md").exists(),
            "the typed name renamed the file on disk"
        );
        assert!(!dir.path().join("a.txt").exists(), "the old name is gone");
    }

    /// Dragging a folder out of the list and dropping it on the docked sidebar
    /// pins it to Favourites; a release off the sidebar pins nothing.
    #[test]
    fn dropping_a_folder_on_the_sidebar_pins_it() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
        let before = h.w.favorites().len();
        let folder = PathBuf::from("/home/tester/Projects/tilo");

        // Begin the drag (what the file view emits once a folder press travels).
        assert!(h.w.apply_command(FileCommand::BeginDrag {
            path: folder.clone(),
            is_dir: true
        }));
        assert!(h.w.debug_pin_dragging(), "the drag is in flight");

        // Move over the sidebar, then release → pin.
        let sb = h.w.debug_sidebar_rect();
        h.cursor(sb.center().x, sb.center().y);
        h.left_release();
        assert!(!h.w.debug_pin_dragging(), "the drop ends the drag");
        assert_eq!(h.w.favorites().len(), before + 1, "the folder was pinned");
        assert!(
            h.w.favorites().iter().any(|f| f.path == folder),
            "and it is the dropped folder"
        );
    }

    #[test]
    fn releasing_a_pin_drag_off_the_sidebar_pins_nothing() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        let before = h.w.favorites().len();
        h.w.apply_command(FileCommand::BeginDrag {
            path: PathBuf::from("/home/tester/Downloads/tmp"),
            is_dir: true,
        });
        // Release over the content pane (not the sidebar).
        let content = h.w.debug_content_rect();
        h.cursor(content.center().x, content.center().y);
        h.left_release();
        assert!(!h.w.debug_pin_dragging());
        assert_eq!(
            h.w.favorites().len(),
            before,
            "a drop off the sidebar pins nothing"
        );
    }

    /// A press on a row, a frame (the click changed the selection, so the
    /// content pane — and the view that saw the press — is rebuilt), then
    /// a move past the threshold: the drag must start anyway. This is the
    /// real mouse sequence; state parked only in the view was lost between
    /// press and move, and no drag ever began from a fresh click.
    #[test]
    fn a_row_press_still_becomes_a_drag_after_the_view_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("photo.png"), b"png").unwrap();
        let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
        let view = h.rect_of("FileView").expect("the file view is laid out");
        // First list row: below the header band.
        let row = (view.x0 + 60.0, view.y0 + 30.0 + 17.0);
        h.cursor(row.0, row.1);
        h.left_press();
        assert!(!h.w.debug_pin_dragging(), "a press alone is not a drag");
        // The frame after the click rebuilds the content pane.
        h.lay_out(1200.0, 720.0);
        h.cursor(row.0 + 40.0, row.1 + 20.0);
        assert!(
            h.w.debug_pin_dragging(),
            "the armed press must start the drag after the rebuild"
        );
        assert_eq!(
            crate::external_drag::unclaimed().map(|d| d.paths),
            Some(vec![dir.path().join("photo.png")]),
            "the host is told what is being carried"
        );
        h.left_release();
        assert!(!h.w.debug_pin_dragging());
        assert_eq!(crate::external_drag::unclaimed(), None);
    }

    /// A right-click on a sidebar favourite opens *its* menu (Open + Remove from
    /// Favourites), not the file-view context menu. The pinned Home entry offers
    /// no Remove. Drives the real right-press path end to end.
    #[test]
    fn right_clicking_a_favourite_opens_its_menu() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            ShellMode::Wide,
            "wide docks the sidebar"
        );

        let band =
            h.w.favorites_store()
                .borrow()
                .band
                .expect("sidebar laid out");
        // Row 1 is a removable favourite (row 0 is the pinned Home).
        let (x, y) = (band.center_x(), band.row_center_y(1));
        h.cursor(x, y);
        h.right_press();
        let labels = h.open_menu_labels();
        assert!(
            labels.iter().any(|l| l == "Remove from Favourites"),
            "the favourite's own menu is shown: {labels:?}",
        );
        assert!(
            labels.iter().any(|l| l == "Open"),
            "with the Open folder action: {labels:?}"
        );
        // It is NOT the file-view context menu (which would offer New Folder).
        assert!(
            !labels.iter().any(|l| l == "New Folder"),
            "not the file menu: {labels:?}"
        );

        // The pinned Home favourite (row 0) has no Remove.
        let (hx, hy) = (band.center_x(), band.row_center_y(0));
        h.cursor(hx, hy);
        h.right_press();
        let home = h.open_menu_labels();
        assert!(
            !home.iter().any(|l| l == "Remove from Favourites"),
            "Home is pinned and cannot be removed: {home:?}",
        );
    }

    #[test]
    fn wide_docks_the_places_list_beside_the_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
        assert!(h.w.debug_places_open(), "wide docks the places list open");

        let content = h.w.debug_content_rect();
        assert!(
            (content.x1 - 1200.0).abs() <= f64::EPSILON,
            "the file view runs to the window's right edge",
        );
        assert!(
            content.x0 > 0.0,
            "and starts after the docked places list: {content:?}",
        );

        crate::command::push(&h.w.cmds, FileCommand::Navigate(dir.path().join("sub")));
        h.cursor(10.0, 10.0);
        assert!(
            h.w.debug_places_open(),
            "wide navigation leaves the docked list in place — nothing to dismiss",
        );
    }
}

#[cfg(test)]
mod accessibility_tests {
    use super::*;
    use ui::a11y::{Politeness, Role};

    fn empty_widget() -> (tempfile::TempDir, FileExplorerWidget) {
        let home = tempfile::tempdir().unwrap();
        let mut widget =
            FileExplorerWidget::with_persistence(None, Some(home.path().to_path_buf()));
        widget.entries.clear();
        widget.error = None;
        widget.is_loading = false;
        (home, widget)
    }

    #[test]
    fn custom_empty_loading_and_error_states_are_live_semantics() {
        let (_home, mut widget) = empty_widget();
        let empty = Widget::accessibility(&widget).expect("Files has a live root");
        assert_eq!(empty.role, Role::Group);
        assert_eq!(empty.name.as_deref(), Some("Files activity"));
        assert_eq!(empty.live, Some(Politeness::Polite));
        assert_eq!(empty.value.as_deref(), Some("This folder is empty"));
        assert!(!empty.state.busy);

        widget.is_loading = true;
        let loading = Widget::accessibility(&widget).unwrap();
        assert!(loading.state.busy);
        assert!(loading.value.as_deref().unwrap().starts_with("Loading "));
        assert_eq!(loading.children.len(), 1);
        assert_eq!(loading.children[0].name.as_deref(), Some("Opening folder"));
        assert_eq!(loading.children[0].live, Some(Politeness::Polite));

        widget.is_loading = false;
        widget.error = Some("permission denied".into());
        let error = Widget::accessibility(&widget).unwrap();
        assert_eq!(error.live, Some(Politeness::Assertive));
        assert!(
            error
                .value
                .as_deref()
                .unwrap()
                .contains("permission denied")
        );
        assert!(error.value.as_deref().unwrap().contains("Try Again"));
        let state = error.children.first().expect("error state semantic group");
        assert_eq!(state.name.as_deref(), Some("Couldn’t open this folder"));
        assert_eq!(state.live, Some(Politeness::Assertive));
        let retry = state.children.first().expect("retry semantic action");
        assert_eq!(retry.role, Role::Button);
        assert_eq!(retry.action_target.as_deref(), Some("files-retry-folder"));
    }

    #[test]
    fn deterministic_shot_seams_cover_loading_and_actionable_error_states() {
        let (_home, mut widget) = empty_widget();
        widget.debug_show_loading_state();
        assert!(widget.is_loading());
        assert!(widget.error().is_none());
        assert!(widget.entries().is_empty());

        widget.debug_show_error_state();
        assert!(!widget.is_loading());
        assert!(
            widget
                .error()
                .is_some_and(|error| error.contains("Permission denied"))
        );
        assert!(widget.entries().is_empty());
    }

    #[test]
    fn collaboration_sheet_is_a_focused_modal_semantic_subtree() {
        let (_home, mut widget) = empty_widget();
        widget.show_collab_panel(CollabPanel {
            history: None,
            title: "Sharing — report.pdf".into(),
            summary: "Shared with 2 people".into(),
            lines: vec!["Alice can view".into(), "Bob can edit".into()],
            action: None,
        });

        let dialog = Widget::accessibility(&widget).expect("open sheet has semantics");
        assert_eq!(dialog.role, Role::Dialog);
        assert_eq!(dialog.name.as_deref(), Some("Sharing — report.pdf"));
        assert!(dialog.state.modal && dialog.state.focusable && dialog.state.focused);
        assert_eq!(dialog.children[0].role, Role::Heading { level: 2 });
        assert_eq!(dialog.children[1].role, Role::Paragraph);
        assert!(dialog.value.as_deref().unwrap().contains("Bob can edit"));
        assert!(Widget::traps_focus(&widget));
        assert!(
            Widget::children(&widget).is_empty(),
            "background toolbar/sidebar/list leave the semantic tree while modal"
        );

        widget.close_collab_panel();
        assert!(!Widget::traps_focus(&widget));
        assert!(!Widget::children(&widget).is_empty());
        assert_eq!(Widget::accessibility(&widget).unwrap().role, Role::Group);
    }

    #[test]
    #[cfg(unix)]
    fn destructive_eviction_requires_a_named_modal_action_before_writing_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("only-copy.txt");
        std::fs::write(&file, b"only local copy").unwrap();
        let (_home, mut widget) = empty_widget();
        widget.show_collab_panel(CollabPanel {
            history: None,
            title: t("files-destructive-eviction-title"),
            summary: t_args("files-destructive-eviction-summary", &[arg("count", 1_i64)]),
            lines: vec![t("files-destructive-eviction-loss")],
            action: Some(CollabPanelAction::DestructiveEviction(vec![file.clone()])),
        });

        let dialog = Widget::accessibility(&widget).expect("warning dialog has semantics");
        let action = dialog.children.last().expect("dialog action");
        assert_eq!(action.role, Role::Button);
        assert_eq!(
            action.name.as_deref(),
            Some(t("files-destructive-eviction-confirm").as_str())
        );
        assert_eq!(
            xattr::get(&file, crate::model::AVAILABILITY_CONTROL_XATTR).unwrap(),
            None,
            "opening the warning must not perform the destructive operation"
        );

        let command = widget
            .collab_panel_action_command()
            .expect("the explicit action is keyboard/click routable");
        widget.close_collab_panel();
        assert!(widget.apply_command(command));
        assert_eq!(
            xattr::get(&file, crate::model::AVAILABILITY_CONTROL_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"evict-destructive".as_slice())
        );
    }
}

#[cfg(test)]
mod breadcrumb_home_tests {
    use super::*;

    /// The window opens on the user's real home, and the breadcrumb has
    /// to be measured against the same one.
    ///
    /// Under a sandbox this process's `HOME` is its own per-package
    /// directory (`/run/tilo/app`) while the directory actually opened is
    /// the user's real home, bound by a grant and named by
    /// `TILO_USER_HOME`. `new` resolved the start directory from the
    /// second and the breadcrumb's home from the first, so
    /// [`crate::model::format_path`] was measuring against a directory
    /// nothing is under — and the trail rendered as an absolute path
    /// from the filesystem root instead of a `~`.
    #[test]
    fn breadcrumb_home_is_the_granted_user_home_not_the_process_home() {
        let granted = tempfile::tempdir().expect("tempdir");
        let granted_path = granted.path().to_path_buf();
        // Only `TILO_USER_HOME` is touched: `HOME` is left as the machine's,
        // which is precisely the disagreement under test — the override has
        // to win over whatever `dirs::home_dir()` says, and it does not say
        // this temp dir.
        // SAFETY: the value is read on the two lines below and by nothing
        // else in this crate's tests.
        unsafe { std::env::set_var("TILO_USER_HOME", &granted_path) };

        let resolved = crate::model::user_home();
        let widget_home = FileExplorerWidget::new()
            .home_path()
            .map(std::path::Path::to_path_buf);

        unsafe { std::env::remove_var("TILO_USER_HOME") };

        assert_eq!(
            resolved.as_deref(),
            Some(granted_path.as_path()),
            "the granted home wins over the process's own HOME",
        );
        assert_ne!(
            dirs::home_dir().as_deref(),
            Some(granted_path.as_path()),
            "this test only means something while the two disagree",
        );
        assert_eq!(
            widget_home.as_deref(),
            Some(granted_path.as_path()),
            "the widget measures its breadcrumb against the home it opens on",
        );

        // Which is what makes the trail read as somebody's home rather
        // than as a path from the filesystem root. This is the function
        // that rendered the bad one: handed a home the path is not
        // under, it falls through to the `/`-rooted branch and emits a
        // component per directory from the filesystem root.
        let deep = granted_path.join("Documents").join("Investors");
        let labels: Vec<String> =
            crate::breadcrumbs::breadcrumb_components(&deep, widget_home.as_deref(), &[])
                .into_iter()
                .map(|(label, _)| label)
                .collect();
        assert_eq!(
            labels,
            vec!["Home", "Documents", "Investors"],
            "the trail is rooted at Home, not at /",
        );

        assert_eq!(
            crate::model::format_path(&granted_path.join("Documents"), widget_home.as_deref()),
            "~/Documents",
        );
        assert_eq!(
            crate::model::format_path(&granted_path, widget_home.as_deref()),
            "~",
        );
    }
}

#[cfg(test)]
mod injected_startup_tests {
    use super::*;
    use crate::model::fixture_io_guard::{DenyAmbientIo, OwnedListings};

    struct ClearOpener;
    impl Drop for ClearOpener {
        fn drop(&mut self) {
            clear_document_opener();
        }
    }

    fn startup(path: &Path) -> StartupInputs {
        StartupInputs {
            initial_dir: path.to_path_buf(),
            workspace_roots: Vec::new(),
            record_roots: Vec::new(),
            remote_accounts: Vec::new(),
            trusted_humans: crate::identity_directory::TrustedHumanDirectory::default(),
        }
    }

    fn absent_services() -> HostedFileServices {
        HostedFileServices {
            thumbnails: None,
            document_opener: None,
        }
    }

    fn owned_child(parent: &Path) -> PathBuf {
        let child = parent.join("files");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("Welcome.txt"), b"Shell capture fixture\n").unwrap();
        child
    }

    #[test]
    fn injected_startup_and_reload_keep_the_host_directory_and_catalogs() {
        let parent = tempfile::tempdir().unwrap();
        let child = owned_child(parent.path());
        let guard = DenyAmbientIo::new();
        let listings = OwnedListings::new(&child);
        let account = crate::remote_accounts::RemoteAccount {
            id: accounts::AccountId::from("host-fixture".to_string()),
            kind: crate::remote_accounts::RemoteAccountKind::WebDav,
            label: "Host fixture".to_string(),
            host: "fixture.invalid".to_string(),
            health: crate::remote_accounts::RemoteAccountHealth::Disabled,
        };
        let mut inputs = startup(&child);
        inputs.remote_accounts.push(account.clone());
        let mut widget =
            FileExplorerWidget::with_startup_inputs_and_services(inputs, absent_services());
        assert_eq!(widget.current_path, child);
        assert_eq!(widget.displayed_entries().len(), 1);
        assert_eq!(widget.displayed_entries()[0].name, "Welcome.txt");
        ui::widget::Widget::set_focus(&mut widget, true);
        let mut document = automerge::AutoCommit::new();
        ui::widget::Widget::sync(&mut widget, &mut document, &automerge::ROOT);
        widget.refresh_external_search_catalogs();
        assert!(!widget.refresh_trusted_identities(None));
        widget.refresh_metadata_if_due(Instant::now() + metadata_refresh::LIVE_INTERVAL);
        widget.refresh();
        assert_eq!(widget.current_path, child);
        assert!(widget.workspace_roots.is_empty());
        assert_eq!(widget.remote_account_snapshot(), vec![account]);
        assert_eq!(
            widget.trusted_humans,
            crate::identity_directory::TrustedHumanDirectory::default()
        );
        assert_eq!(guard.attempts(), 0);
        assert!(
            listings
                .observations()
                .iter()
                .all(|(path, workspace)| path == &child && !workspace)
        );
    }

    #[test]
    fn injected_services_absence_never_builds_thumbnails_or_uses_global_opener() {
        let parent = tempfile::tempdir().unwrap();
        let child = owned_child(parent.path());
        let guard = DenyAmbientIo::new();
        let listings = OwnedListings::new(&child);
        let calls = Rc::new(Cell::new(0));
        set_document_opener({
            let calls = calls.clone();
            move |_| {
                calls.set(calls.get() + 1);
                Err("test TLS opener must remain unused".into())
            }
        });
        let _clear = ClearOpener;
        let mut widget = FileExplorerWidget::with_startup_inputs_and_services(
            startup(&child),
            absent_services(),
        );
        assert!(widget.thumbs.is_none());
        crate::command::push(&widget.cmds, FileCommand::Activate(0));
        assert!(widget.drain_commands());
        assert_eq!(
            widget.status_line(),
            Some("Document opening is unavailable")
        );
        assert_eq!(calls.get(), 0);
        assert_eq!(guard.attempts(), 0);
        assert_eq!(listings.observations(), vec![(child, false)]);
    }

    #[test]
    fn injected_listing_and_refresh_use_only_explicit_workspace_roots() {
        let parent = tempfile::tempdir().unwrap();
        let child = owned_child(parent.path());
        let guard = DenyAmbientIo::new();
        let listings = OwnedListings::new(&child);
        let mut inputs = startup(&child);
        inputs
            .workspace_roots
            .push(parent.path().join("unrelated-workspace"));
        let mut widget =
            FileExplorerWidget::with_startup_inputs_and_services(inputs, absent_services());
        std::fs::write(child.join("Reload.txt"), b"reload").unwrap();
        assert!(widget.apply_command(FileCommand::Refresh));
        assert_eq!(widget.displayed_entries().len(), 2);
        std::fs::write(child.join("Metadata.txt"), b"metadata").unwrap();
        assert!(widget.refresh_metadata_preserving_view());
        assert_eq!(widget.displayed_entries().len(), 3);
        assert_eq!(
            listings.observations(),
            vec![
                (child.clone(), false),
                (child.clone(), false),
                (child, false)
            ]
        );
        assert_eq!(guard.attempts(), 0);
    }

    #[test]
    fn fixture_io_guards_stop_before_following_io() {
        let parent = tempfile::tempdir().unwrap();
        let child = owned_child(parent.path());
        let guard = DenyAmbientIo::new();
        let listings = OwnedListings::new(&child);
        let reached = Cell::new(0);
        let denied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::model::fixture_io_guard::ambient_io("guard negative control");
            reached.set(reached.get() + 1);
        }));
        assert!(denied.is_err());
        assert_eq!(reached.get(), 0);
        assert_eq!(guard.attempts(), 1);
        let denied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::model::fixture_io_guard::observe_owned_listing(
                &parent.path().join("sibling"),
                false,
            );
            reached.set(reached.get() + 1);
        }));
        assert!(denied.is_err());
        assert_eq!(reached.get(), 0);
        assert!(listings.observations().is_empty());
    }
}

#[test]
fn headless_f2_renames_fixture_without_opening_a_file() {
    struct RestoreOpener;
    impl Drop for RestoreOpener {
        fn drop(&mut self) {
            clear_document_opener();
        }
    }
    let _restore = RestoreOpener;
    let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = opened.clone();
    set_document_opener(move |path| {
        sink.borrow_mut().push(path.to_path_buf());
        Err("unexpected external open during rename regression".into())
    });
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("report.txt");
    std::fs::write(&original, b"preserved bytes").unwrap();
    let mut widget = FileExplorerWidget::with_persistence(None, None);
    widget.navigate(directory.path().to_path_buf());
    let mut driver =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(widget), (1280, 800));
    let mut previous = original;
    for name in ["renamed.txt", "RENAMED.txt"] {
        driver
            .perform_named("file_view", &ui::a11y::Action::Focus)
            .unwrap();
        driver.key("Home");
        driver.key("F2");
        driver.set_modifiers(&["ctrl".into()]);
        driver.key("a");
        driver.set_modifiers(&[]);
        driver.type_text(name);
        driver.key("Enter");
        assert!(
            opened.borrow().is_empty(),
            "F2 must enter rename before Enter is sent"
        );
        let destination = directory.path().join(name);
        assert_eq!(std::fs::read(&destination).unwrap(), b"preserved bytes");
        let names = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from(name)]);
        if previous.file_name() != destination.file_name() && name == "renamed.txt" {
            assert!(!previous.exists());
        }
        previous = destination;
    }
}
