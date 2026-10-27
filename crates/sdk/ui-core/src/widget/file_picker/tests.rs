use super::*;
use crate::testing::{Dispatch, dispatch};
use automerge::AutoCommit;
use std::fs;
use winit::event::ElementState;

fn cursor_at(p: Point) -> WindowEvent {
    crate::synthetic_input::cursor_moved(p.x, p.y)
}

use crate::synthetic_input::mouse_press;

/// Exercises the real `std::fs::read_dir` path rather than a mock.
fn temp_tree() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    fs::write(root.join("alpha.txt"), b"a").unwrap();
    fs::write(root.join("beta.txt"), b"b").unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("nested").join("gamma.txt"), b"c").unwrap();
    (dir, root)
}

#[test]
fn picker_opens_as_modal_and_resolves_open_or_cancel() {
    fn accessibility_reports_modal_dialog_with_title_name() {
        use crate::a11y::Role;
        let (_g, root) = temp_tree();
        let picker = FilePicker::new("Open file", &root, vec![root.clone()]).settled();
        let node = Widget::accessibility(&picker).expect("file picker is a dialog");
        assert_eq!(node.role, Role::Dialog);
        assert_eq!(node.name.as_deref(), Some("Open file"));
        assert!(node.state.modal, "the file chooser traps focus");
    }

    fn new_picker_reads_initial_dir_entries() {
        let (_g, root) = temp_tree();
        let picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        // nested (dir), alpha.txt, beta.txt — directories sort first.
        assert_eq!(picker.entries().len(), 3);
        assert!(picker.entries()[0].is_dir);
        assert_eq!(picker.entries()[0].name, "nested");
    }

    fn cancel_returns_none() {
        // Cancelled is what the host translates to `oneshot::send(None)`
        // for the FileChooser response.
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.click_cancel();
        let outcome = picker.take_outcome();
        assert_eq!(outcome, Some(FilePickerOutcome::Cancelled));
        assert!(picker.take_outcome().is_none());
    }

    fn open_returns_selected_paths() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        // Pick `alpha.txt` (after directories, so index 1).
        let target = root.join("alpha.txt");
        assert!(picker.select_by_path(&target));
        picker.click_open();
        settle(&mut picker);
        let outcome = picker.take_outcome();
        assert_eq!(outcome, Some(FilePickerOutcome::Selected(vec![target])));
    }

    fn open_with_no_selection_is_disabled_and_does_not_cancel_the_dialog() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.click_open();
        settle(&mut picker);
        assert!(picker.take_outcome().is_none());
        let node = Widget::accessibility(&picker).expect("dialog semantics");
        let open = node
            .children
            .iter()
            .find(|child| child.name.as_deref() == Some("Open"))
            .expect("Open action");
        assert!(open.state.disabled);
        assert!(!open.state.focusable);
    }

    fn escape_cancels() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Escape));
        settle(&mut picker);
        assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
    }

    accessibility_reports_modal_dialog_with_title_name();
    new_picker_reads_initial_dir_entries();
    cancel_returns_none();
    open_returns_selected_paths();
    open_with_no_selection_is_disabled_and_does_not_cancel_the_dialog();
    escape_cancels();
}

#[test]
fn directory_mode_lists_selects_and_chooses_folders() {
    fn directory_mode_can_choose_the_current_empty_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let mut picker = FilePicker::new("Choose folder", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();

        picker.click_open();
        settle(&mut picker);

        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root]))
        );
    }

    fn directory_mode_lists_only_folders_and_chooses_a_selected_child() {
        let (_guard, root) = temp_tree();
        let child = root.join("nested");
        let mut picker = FilePicker::new("Choose folder", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();

        assert_eq!(picker.entries().len(), 1);
        assert_eq!(picker.entries()[0].path, child);
        assert!(picker.entries()[0].is_dir);
        assert!(!picker.select_by_path(&root.join("alpha.txt")));
        assert!(picker.select_by_path(&child));
        picker.click_open();
        settle(&mut picker);

        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![child]))
        );
    }

    fn directory_mode_single_and_multiple_selection_are_enforced_in_the_widget() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let first = root.join("a");
        let second = root.join("b");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();

        let mut single = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .with_multiple(false)
            .settled();
        single.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        for index in [0, 1] {
            let center = single.entry_rect(index).unwrap().center();
            dispatch(&mut single, &cursor_at(center));
            settle(&mut single);
            Dispatch::new()
                .modifiers(ctrl())
                .send(&mut single, &mouse_press());
            settle(&mut single);
        }
        assert_eq!(single.selected_indices(), vec![1]);

        let mut multiple = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .with_multiple(true)
            .settled();
        multiple.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        for index in [0, 1] {
            let center = multiple.entry_rect(index).unwrap().center();
            dispatch(&mut multiple, &cursor_at(center));
            settle(&mut multiple);
            Dispatch::new()
                .modifiers(ctrl())
                .send(&mut multiple, &mouse_press());
            settle(&mut multiple);
        }
        assert_eq!(multiple.selected_indices(), vec![0, 1]);

        let mut keyboard_single = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .with_multiple(false)
            .settled();
        keyboard_single.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        dispatch(
            &mut keyboard_single,
            &key_named(winit::keyboard::NamedKey::ArrowDown),
        );
        settle(&mut keyboard_single);
        Dispatch::new().modifiers(shift()).send(
            &mut keyboard_single,
            &key_named(winit::keyboard::NamedKey::ArrowDown),
        );
        settle(&mut keyboard_single);
        assert_eq!(keyboard_single.selected_indices(), vec![1]);
    }

    fn directory_mode_uses_custom_and_default_labels_with_folder_semantics() {
        use crate::a11y::Role;

        let (_guard, root) = temp_tree();
        let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .with_filters(vec![FileFilter::new("Images", &["png"]), FileFilter::all()])
            .with_accept_label(Some("Attach folder".into()))
            .settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        let node = Widget::accessibility(&picker).expect("dialog semantics");
        assert!(
            node.children.iter().any(|child| {
                child.role == Role::List && child.name.as_deref() == Some("Folders")
            })
        );
        assert!(node.children.iter().any(|child| {
            child.role == Role::Button && child.name.as_deref() == Some("Attach folder")
        }));
        assert!(
            !node.children.iter().any(|child| {
                child.key == Some(PickerFocus::Filename.id())
                    || child.key == Some(PickerFocus::Filter.id())
            }),
            "a folder has no name to type and no type to pick"
        );

        let default = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .with_accept_label(Some("   ".into()))
            .settled();
        let node = Widget::accessibility(&default).expect("dialog semantics");
        assert!(node.children.iter().any(|child| {
            child.role == Role::Button && child.name.as_deref() == Some("Choose folder")
        }));
    }

    fn single_open_mode_constrains_selection_without_changing_the_default() {
        let (_guard, root) = temp_tree();
        let mut single = FilePicker::new("Open", &root, vec![root.clone()])
            .with_multiple(false)
            .settled();
        single.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        for index in [1, 2] {
            let center = single.entry_rect(index).unwrap().center();
            dispatch(&mut single, &cursor_at(center));
            settle(&mut single);
            Dispatch::new()
                .modifiers(ctrl())
                .send(&mut single, &mouse_press());
            settle(&mut single);
        }
        assert_eq!(single.selected_indices(), vec![2]);

        let mut default_multiple = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        default_multiple.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        for index in [1, 2] {
            let center = default_multiple.entry_rect(index).unwrap().center();
            dispatch(&mut default_multiple, &cursor_at(center));
            settle(&mut default_multiple);
            Dispatch::new()
                .modifiers(ctrl())
                .send(&mut default_multiple, &mouse_press());
            settle(&mut default_multiple);
        }
        assert_eq!(default_multiple.selected_indices(), vec![1, 2]);
    }

    directory_mode_can_choose_the_current_empty_folder();
    directory_mode_lists_only_folders_and_chooses_a_selected_child();
    directory_mode_single_and_multiple_selection_are_enforced_in_the_widget();
    directory_mode_uses_custom_and_default_labels_with_folder_semantics();
    single_open_mode_constrains_selection_without_changing_the_default();
}

#[test]
fn directory_mode_pointer_and_keyboard_confirmation() {
    fn directory_mode_click_selects_and_double_click_navigates() {
        let (_guard, root) = temp_tree();
        let child = root.join("nested");
        let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        let center = picker.entry_rect(0).unwrap().center();
        dispatch(&mut picker, &cursor_at(center));
        settle(&mut picker);

        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.current_dir(), root.as_path());
        assert_eq!(picker.selection(), Some(0));

        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.current_dir(), child.as_path());
    }

    fn directory_mode_mouse_selects_and_footer_confirms_a_child() {
        let (_guard, root) = temp_tree();
        let child = root.join("nested");
        let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));

        let row = picker.entry_rect(0).unwrap().center();
        dispatch(&mut picker, &cursor_at(row));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        let confirm = picker.open_rect().center();
        dispatch(&mut picker, &cursor_at(confirm));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);

        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![child]))
        );
    }

    fn directory_mode_keyboard_can_navigate_and_confirm() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let apple = root.join("apple");
        let cherry = root.join("cherry");
        fs::create_dir(&apple).unwrap();
        fs::create_dir(&cherry).unwrap();
        let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));

        dispatch(&mut picker, &key_char("c"));
        settle(&mut picker);
        assert_eq!(picker.selected_paths(), std::slice::from_ref(&cherry));
        assert_eq!(picker.focus, Some(PickerFocus::Search));
        for _ in 0..3 {
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            settle(&mut picker);
        }
        assert_eq!(picker.focus, Some(PickerFocus::Confirm));
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
        settle(&mut picker);

        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![cherry]))
        );

        let mut navigate = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();
        navigate.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        dispatch(
            &mut navigate,
            &key_named(winit::keyboard::NamedKey::ArrowDown),
        );
        settle(&mut navigate);
        dispatch(&mut navigate, &key_named(winit::keyboard::NamedKey::Enter));
        settle(&mut navigate);
        assert_eq!(navigate.current_dir(), apple.as_path());
    }

    fn directory_mode_empty_folder_keyboard_confirmation_and_cancel_remain_usable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();

        // An empty folder starts on search; Cancel and the choice follow.
        assert_eq!(picker.focus, Some(PickerFocus::Search));
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Cancel));
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Confirm));
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.clone()]))
        );

        let mut cancelled = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();
        dispatch(
            &mut cancelled,
            &key_named(winit::keyboard::NamedKey::Escape),
        );
        settle(&mut cancelled);
        assert_eq!(cancelled.take_outcome(), Some(FilePickerOutcome::Cancelled));
    }

    directory_mode_click_selects_and_double_click_navigates();
    directory_mode_mouse_selects_and_footer_confirms_a_child();
    directory_mode_keyboard_can_navigate_and_confirm();
    directory_mode_empty_folder_keyboard_confirmation_and_cancel_remain_usable();
}

#[test]
fn directory_confirmation_revalidates_the_filesystem() {
    fn directory_confirmation_refuses_a_vanished_or_replaced_selection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let child = root.join("child");
        fs::create_dir(&child).unwrap();
        let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();
        assert!(picker.select_by_path(&child));

        fs::remove_dir(&child).unwrap();
        fs::write(&child, b"now a file").unwrap();
        picker.click_open();
        settle(&mut picker);

        assert!(picker.take_outcome().is_none());
        picker.click_cancel();
        assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
    }

    fn directory_confirmation_rechecks_the_current_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let current = root.join("current");
        fs::create_dir(&current).unwrap();
        let mut picker = FilePicker::new("Choose", &current, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();

        fs::remove_dir(&current).unwrap();
        fs::write(&current, b"now a file").unwrap();
        picker.click_open();
        settle(&mut picker);

        assert!(picker.take_outcome().is_none());
    }

    #[cfg(unix)]
    fn directory_confirmation_refuses_a_selection_changed_to_an_escape_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        let root = dir.path().to_path_buf();
        let child = root.join("child");
        fs::create_dir(&child).unwrap();
        let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();
        assert!(picker.select_by_path(&child));

        fs::remove_dir(&child).unwrap();
        symlink(outside.path(), &child).unwrap();
        picker.click_open();
        settle(&mut picker);

        assert!(picker.take_outcome().is_none());
    }

    #[cfg(target_os = "linux")]
    fn directory_selection_preserves_non_utf8_native_paths() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let child = root.join(OsStr::from_bytes(b"folder-\xff"));
        fs::create_dir(&child).unwrap();
        let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Directory)
            .settled();
        assert!(picker.select_by_path(&child));

        picker.click_open();
        settle(&mut picker);

        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![child]))
        );
    }

    directory_confirmation_refuses_a_vanished_or_replaced_selection();
    directory_confirmation_rechecks_the_current_directory();
    #[cfg(unix)]
    directory_confirmation_refuses_a_selection_changed_to_an_escape_symlink();
    #[cfg(target_os = "linux")]
    directory_selection_preserves_non_utf8_native_paths();
}

#[test]
fn navigation_stays_under_roots() {
    fn navigate_into_subdir_updates_current_dir_and_entries() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        let nested = root.join("nested");
        picker.navigate_to(&nested);
        settle(&mut picker);
        assert_eq!(picker.current_dir(), nested.as_path());
        assert_eq!(picker.entries().len(), 1);
        assert_eq!(picker.entries()[0].name, "gamma.txt");
    }

    fn navigate_outside_roots_is_silently_dropped() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        let cwd_before = picker.current_dir().to_path_buf();
        picker.navigate_to(Path::new("/etc"));
        settle(&mut picker);
        assert_eq!(picker.current_dir(), cwd_before.as_path());
    }

    fn can_navigate_to_requires_under_a_root() {
        let (_g, root) = temp_tree();
        let picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        assert!(picker.can_navigate_to(&root));
        assert!(picker.can_navigate_to(&root.join("nested")));
        assert!(!picker.can_navigate_to(Path::new("/etc")));
    }

    fn up_button_navigates_to_parent() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        let nested = root.join("nested");
        picker.navigate_to(&nested);
        settle(&mut picker);
        assert_eq!(picker.current_dir(), nested.as_path());
        assert!(picker.can_go_up());
        picker.go_up();
        settle(&mut picker);
        assert_eq!(picker.current_dir(), root.as_path());
    }

    navigate_into_subdir_updates_current_dir_and_entries();
    navigate_outside_roots_is_silently_dropped();
    can_navigate_to_requires_under_a_root();
    up_button_navigates_to_parent();
}

#[test]
fn pointer_rows_and_footer_buttons_navigate_select_and_resolve() {
    fn click_on_directory_row_navigates_into_it() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        // Row 0 is the `nested` directory (dirs sort first).
        let center = picker.entry_rect(0).expect("row 0").center();
        dispatch(&mut picker, &cursor_at(center));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.current_dir(), root.join("nested").as_path());
    }

    fn click_on_file_row_sets_selection() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        let center = picker.entry_rect(1).expect("row 1").center();
        dispatch(&mut picker, &cursor_at(center));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.selection(), Some(1));
    }

    fn click_cancel_button_resolves_cancelled() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        let center = picker.cancel_rect().center();
        dispatch(&mut picker, &cursor_at(center));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
    }

    fn click_open_button_with_selection_resolves_selected() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        let target = root.join("alpha.txt");
        picker.select_by_path(&target);
        let center = picker.open_rect().center();
        dispatch(&mut picker, &cursor_at(center));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![target]))
        );
    }

    fn ctrl_click_multi_select_opens_all() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        // entries: [nested(dir), alpha.txt(1), beta.txt(2)]. Select both files.
        let r1 = picker.entry_rect(1).unwrap().center();
        dispatch(&mut picker, &cursor_at(r1));
        settle(&mut picker);
        Dispatch::new()
            .modifiers(ctrl())
            .send(&mut picker, &mouse_press());
        settle(&mut picker);
        let r2 = picker.entry_rect(2).unwrap().center();
        dispatch(&mut picker, &cursor_at(r2));
        settle(&mut picker);
        Dispatch::new()
            .modifiers(ctrl())
            .send(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.selected_indices(), vec![1, 2]);
        picker.click_open();
        settle(&mut picker);
        match picker.take_outcome() {
            Some(FilePickerOutcome::Selected(paths)) => {
                assert_eq!(paths.len(), 2);
                assert!(paths.contains(&root.join("alpha.txt")));
                assert!(paths.contains(&root.join("beta.txt")));
            }
            other => panic!("expected two selected paths, got {other:?}"),
        }
    }

    fn double_click_file_opens_immediately() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        let center = picker.entry_rect(1).unwrap().center();
        dispatch(&mut picker, &cursor_at(center));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press()); // first click selects
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press()); // second click opens
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
        );
    }

    click_on_directory_row_navigates_into_it();
    click_on_file_row_sets_selection();
    click_cancel_button_resolves_cancelled();
    click_open_button_with_selection_resolves_selected();
    ctrl_click_multi_select_opens_all();
    double_click_file_opens_immediately();
}

#[test]
fn save_mode_filename_resolves_and_rejects_escapes() {
    fn save_mode_uses_save_label() {
        let (_g, root) = temp_tree();
        let picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .settled();
        // The label itself is only exercised at draw-time; the mode
        // getter isn't public, so this pins the construction path.
        assert!(matches!(picker.mode, FilePickerMode::Save));
    }

    fn save_mode_typed_filename_resolves_target() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .settled();
        for ch in ["n", "e", "w"] {
            dispatch(&mut picker, &key_char(ch));
            settle(&mut picker);
        }
        dispatch(&mut picker, &key_char("."));
        settle(&mut picker);
        for ch in ["t", "x", "t"] {
            dispatch(&mut picker, &key_char(ch));
            settle(&mut picker);
        }
        assert_eq!(picker.filename(), "new.txt");
        // Backspace takes back one character as a person sees it, and an
        // input method's commit lands at the caret.
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::Backspace),
        );
        settle(&mut picker);
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::Backspace),
        );
        settle(&mut picker);
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::Backspace),
        );
        settle(&mut picker);
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::Backspace),
        );
        settle(&mut picker);
        dispatch(&mut picker, &key_char("👍🏽"));
        settle(&mut picker);
        dispatch(&mut picker, &key_char("e\u{301}"));
        settle(&mut picker);
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::Backspace),
        );
        settle(&mut picker);
        assert_eq!(picker.filename(), "new👍🏽");
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::Backspace),
        );
        settle(&mut picker);
        assert_eq!(picker.filename(), "new");
        dispatch(
            &mut picker,
            &WindowEvent::Ime(winit::event::Ime::Preedit("tx".into(), Some((2, 2)))),
        );
        settle(&mut picker);
        assert_eq!(picker.filename(), "new", "a composition is not the value");
        dispatch(
            &mut picker,
            &WindowEvent::Ime(winit::event::Ime::Commit(".txt".into())),
        );
        settle(&mut picker);
        assert_eq!(picker.filename(), "new.txt");
        picker.click_open();
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("new.txt")]))
        );
    }

    fn save_filename_uses_the_same_focus_ring_and_accessibility_target() {
        use crate::a11y::Role;
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        Widget::set_focus(&mut picker, true);
        assert!(crate::ime::ImeWindowState::from_widget(&picker).is_some());
        let original = picker.ime_edit_fingerprint(&[7; 32]).unwrap();
        // Save starts on Filename. Tab leaves it for the next modal control;
        // Shift+Tab returns to the exact editable target.
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Cancel));
        assert!(crate::ime::ImeWindowState::from_widget(&picker).is_none());
        Dispatch::new()
            .modifiers(shift())
            .send(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Filename));
        dispatch(&mut picker, &key_char("x"));
        settle(&mut picker);
        assert_eq!(picker.filename(), "x");
        assert_ne!(picker.ime_edit_fingerprint(&[7; 32]).unwrap(), original);
        assert!(crate::ime::ImeWindowState::from_widget(&picker).is_some());
        let node = Widget::accessibility(&picker).expect("dialog semantics");
        let filename = node
            .children
            .iter()
            .find(|child| {
                child.role == Role::TextInput && child.key == Some(PickerFocus::Filename.id())
            })
            .expect("Filename field");
        assert!(filename.state.focused);
        assert_eq!(filename.value.as_deref(), Some("x"));
    }

    fn save_mode_rejects_absolute_and_traversal_filenames() {
        let (_guard, root) = temp_tree();
        for unsafe_name in [
            "/tmp/stolen",
            "../outside",
            "sub/file",
            ".",
            "..",
            "nested/..",
        ] {
            let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Save)
                .with_filename(unsafe_name)
                .settled();
            picker.click_open();
            settle(&mut picker);
            // Refused names keep the dialog open and say why: a save is
            // never reported as a cancel, and never escapes the scope.
            assert_eq!(
                picker.take_outcome(),
                None,
                "save name {unsafe_name:?} resolved outside the picker scope"
            );
            assert!(
                picker.name_error().is_some(),
                "save name {unsafe_name:?} was refused without a reason"
            );
            assert_eq!(picker.current_dir(), root.as_path());
        }
    }

    fn save_mode_follows_a_typed_path_inside_the_roots() {
        let (_guard, root) = temp_tree();
        let home = root.clone();
        for (typed, folder) in [
            ("nested/report.md", root.join("nested")),
            ("~/nested/notes.md", root.join("nested")),
            ("nested/../top.md", root.clone()),
        ] {
            let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
                .with_home(Some(home.clone()))
                .with_mode(FilePickerMode::Save)
                .with_filename(typed)
                .settled();
            picker.click_open();
            settle(&mut picker);
            let name = Path::new(typed).file_name().unwrap();
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![folder.join(name)])),
                "{typed:?}"
            );
            assert_eq!(picker.current_dir(), folder.as_path(), "{typed:?}");
        }
        // A trailing slash names a folder: open it, keep the dialog up.
        let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .with_filename("nested/")
            .settled();
        picker.click_open();
        settle(&mut picker);
        assert_eq!(picker.take_outcome(), None);
        assert_eq!(picker.current_dir(), root.join("nested").as_path());
        assert_eq!(picker.filename(), "");
        // Fixing the name takes the reason away.
        let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .with_filename("missing/a.txt")
            .settled();
        picker.click_open();
        settle(&mut picker);
        assert!(picker.name_error().is_some());
        let node = Widget::accessibility(&picker).expect("dialog semantics");
        let field = node
            .children
            .iter()
            .find(|child| child.key == Some(PickerFocus::Filename.id()))
            .expect("Filename field");
        assert_eq!(field.role, crate::a11y::Role::TextInput);
        assert!(field.state.invalid);
        Widget::set_focus(&mut picker, true);
        dispatch(&mut picker, &key_char("x"));
        settle(&mut picker);
        assert_eq!(picker.name_error(), None);
    }

    #[cfg(unix)]
    fn save_mode_revalidates_a_symlinked_current_directory() {
        use std::os::unix::fs::symlink;

        let (_guard, root) = temp_tree();
        let outside = tempfile::tempdir().unwrap();
        let escape = root.join("escape");
        symlink(outside.path(), &escape).unwrap();
        let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .with_filename("identity.tilo")
            .settled();
        // Navigation is lexically within `root`; commit must catch where it
        // actually resolves before returning a privileged save target.
        picker.navigate_to(&escape);
        settle(&mut picker);
        picker.click_open();
        settle(&mut picker);
        assert_eq!(picker.take_outcome(), None);
        assert!(picker.name_error().is_some());
    }

    save_mode_uses_save_label();
    save_mode_typed_filename_resolves_target();
    save_filename_uses_the_same_focus_ring_and_accessibility_target();
    save_mode_rejects_absolute_and_traversal_filenames();
    save_mode_follows_a_typed_path_inside_the_roots();
    #[cfg(unix)]
    save_mode_revalidates_a_symlinked_current_directory();
}

#[test]
fn saving_over_an_existing_file_asks_first() {
    use crate::a11y::{Action, Role};
    use winit::keyboard::NamedKey;

    fn save_picker(root: &Path, name: &str) -> FilePicker {
        let mut picker = FilePicker::new("Export", root, vec![root.to_path_buf()])
            .with_mode(FilePickerMode::Save)
            .with_filename(name)
            .settled();
        picker.set_rect(Rect::new(0.0, 0.0, 600.0, 420.0));
        Widget::set_focus(&mut picker, true);
        picker
    }

    fn prompt_node(picker: &FilePicker) -> crate::a11y::AccessNode {
        let dialog = Widget::accessibility(picker).expect("picker semantics");
        assert_eq!(dialog.children.len(), 1, "only the prompt is reachable");
        let prompt = dialog.children[0].clone();
        assert_eq!(prompt.role, Role::Dialog);
        assert!(prompt.state.modal);
        prompt
    }

    fn enter_asks_and_escape_keeps_the_file() {
        let (_g, root) = temp_tree();
        let mut picker = save_picker(&root, "alpha.txt");
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert_eq!(picker.take_outcome(), None, "nothing is chosen yet");
        assert_eq!(
            picker.replace_prompt(),
            Some(root.join("alpha.txt").as_path())
        );
        let prompt = prompt_node(&picker);
        assert_eq!(
            prompt.name.as_deref(),
            Some("A file named \u{201c}alpha.txt\u{201d} already exists here. Replace it?")
        );
        let names: Vec<_> = prompt
            .children
            .iter()
            .map(|button| {
                (
                    button.name.clone().unwrap_or_default(),
                    button.state.focused,
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![("Cancel".to_owned(), true), ("Replace".to_owned(), false)],
            "Cancel holds focus, so Enter alone never replaces"
        );
        // Typing does not reach the name behind the prompt.
        dispatch(&mut picker, &key_char("x"));
        settle(&mut picker);
        assert_eq!(picker.filename(), "alpha.txt");

        dispatch(&mut picker, &key_named(NamedKey::Escape));
        settle(&mut picker);
        assert_eq!(picker.replace_prompt(), None);
        assert_eq!(picker.take_outcome(), None, "Escape leaves the picker open");
        assert_eq!(picker.focus, Some(PickerFocus::Filename));
        assert_eq!(fs::read(root.join("alpha.txt")).unwrap(), b"a");

        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert!(picker.replace_prompt().is_some());
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert_eq!(
            picker.replace_prompt(),
            None,
            "Enter on Cancel keeps the file"
        );
        assert_eq!(picker.take_outcome(), None);
    }

    fn keyboard_reaches_replace() {
        let (_g, root) = temp_tree();
        let mut picker = save_picker(&root, "alpha.txt");
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        dispatch(&mut picker, &key_named(NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Replace));
        Dispatch::new()
            .modifiers(shift())
            .send(&mut picker, &key_named(NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::KeepExisting));
        dispatch(&mut picker, &key_named(NamedKey::ArrowRight));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Replace));
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
        );
    }

    fn pointer_answers_the_prompt() {
        let (_g, root) = temp_tree();
        let mut picker = save_picker(&root, "alpha.txt");
        let at = picker.open_rect().center();
        dispatch(&mut picker, &cursor_at(at));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert!(picker.replace_prompt().is_some(), "Save asks first");
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.take_outcome(), None);
        let at = picker.keep_existing_rect.center();
        dispatch(&mut picker, &cursor_at(at));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.replace_prompt(), None);
        assert_eq!(picker.take_outcome(), None);

        let at = picker.open_rect().center();
        dispatch(&mut picker, &cursor_at(at));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        let at = picker.replace_rect.center();
        dispatch(&mut picker, &cursor_at(at));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
        );
    }

    fn semantic_actions_answer_the_prompt() {
        let (_g, root) = temp_tree();
        let mut picker = save_picker(&root, "alpha.txt");
        let mut doc = AutoCommit::new();
        let path = automerge::ROOT;
        assert!(Widget::perform_action(
            &mut picker,
            &Action::Activate,
            &[PickerFocus::Confirm.id()],
            &mut crate::widget::Edits::lend(&mut doc),
            &path,
        ));
        settle(&mut picker);
        assert!(picker.replace_prompt().is_some());
        assert!(
            !Widget::perform_action(
                &mut picker,
                &Action::Activate,
                &[PickerFocus::Confirm.id()],
                &mut crate::widget::Edits::lend(&mut doc),
                &path,
            ),
            "the picker behind the prompt cannot be operated"
        );
        settle(&mut picker);
        let prompt = prompt_node(&picker);
        let replace = prompt.children[1].key.expect("Replace key");
        assert!(Widget::perform_action(
            &mut picker,
            &Action::Activate,
            &[prompt.key.expect("prompt key"), replace],
            &mut crate::widget::Edits::lend(&mut doc),
            &path,
        ));
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
        );
    }

    fn new_names_save_without_asking_and_folders_open() {
        let (_g, root) = temp_tree();
        let mut picker = save_picker(&root, "fresh.txt");
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("fresh.txt")]))
        );

        let mut picker = save_picker(&root, "nested");
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert_eq!(picker.take_outcome(), None);
        assert_eq!(picker.current_dir(), root.join("nested").as_path());
        assert_eq!(picker.filename(), "");
    }

    fn double_clicking_an_existing_file_asks_too() {
        let (_g, root) = temp_tree();
        let mut picker = save_picker(&root, "");
        let row = picker.entry_rect(1).expect("alpha.txt row").center();
        dispatch(&mut picker, &cursor_at(row));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.take_outcome(), None);
        assert_eq!(
            picker.replace_prompt(),
            Some(root.join("alpha.txt").as_path())
        );
    }

    enter_asks_and_escape_keeps_the_file();
    keyboard_reaches_replace();
    pointer_answers_the_prompt();
    semantic_actions_answer_the_prompt();
    new_names_save_without_asking_and_folders_open();
    double_clicking_an_existing_file_asks_too();
}

#[test]
fn the_name_field_selects_all_like_every_text_field() {
    use winit::keyboard::NamedKey;
    let (_g, root) = temp_tree();
    let mut picker = FilePicker::new("Export", &root, vec![root.clone()])
        .with_mode(FilePickerMode::Save)
        .with_filename("gradient.png")
        .settled();
    picker.set_rect(Rect::new(0.0, 0.0, 600.0, 420.0));
    Widget::set_focus(&mut picker, true);

    Dispatch::new()
        .modifiers(ctrl())
        .send(&mut picker, &key_char("a"));
    settle(&mut picker);
    dispatch(&mut picker, &key_char("x"));
    settle(&mut picker);
    assert_eq!(
        picker.filename(),
        "x",
        "Ctrl+A selects the name, typing replaces it"
    );

    // Away from the field, then three clicks on it select its line.
    dispatch(&mut picker, &key_named(NamedKey::Tab));
    settle(&mut picker);
    let field = picker.filename_rect().center();
    dispatch(&mut picker, &cursor_at(field));
    settle(&mut picker);
    for _ in 0..3 {
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        dispatch(&mut picker, &mouse_release());
        settle(&mut picker);
    }
    dispatch(&mut picker, &key_char("y"));
    settle(&mut picker);
    assert_eq!(
        picker.filename(),
        "y",
        "triple-click selects the whole name"
    );

    assert!(!picker.filename.spellcheck_enabled());
    assert!(!picker.filename.autocorrect_enabled());
}

use crate::synthetic_input::mouse_release;

fn ctrl() -> winit::event::Modifiers {
    let mut s = winit::keyboard::ModifiersState::empty();
    s.set(winit::keyboard::ModifiersState::CONTROL, true);
    winit::event::Modifiers::from(s)
}

fn key_named(named: winit::keyboard::NamedKey) -> WindowEvent {
    crate::synthetic_input::key_input(&winit::keyboard::Key::Named(named), ElementState::Pressed)
}

fn key_char(s: &str) -> WindowEvent {
    crate::synthetic_input::key_input(
        &winit::keyboard::Key::Character(s.into()),
        ElementState::Pressed,
    )
}

fn shift() -> winit::event::Modifiers {
    let mut state = winit::keyboard::ModifiersState::empty();
    state.set(winit::keyboard::ModifiersState::SHIFT, true);
    winit::event::Modifiers::from(state)
}

/// `n` numbered files so the list overflows a short dialog and
/// exercises scrolling.
fn temp_many(n: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    for i in 0..n {
        fs::write(root.join(format!("file{i:03}.txt")), b"x").unwrap();
    }
    (dir, root)
}

#[test]
fn listing_filters_and_scroll_window_rows() {
    fn scrolling_windows_the_visible_rows() {
        let (_g, root) = temp_many(40);
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        // A modest dialog: only a handful of rows fit.
        picker.set_rect(Rect::new(0.0, 0.0, 560.0, 420.0));
        assert!(picker.entry_rect(0).is_some());
        let over_list = cursor_at(picker.list_rect().center());
        dispatch(&mut picker, &over_list);
        settle(&mut picker);
        dispatch(&mut picker, &crate::synthetic_input::mouse_wheel(0.0, -5.0));
        settle(&mut picker);
        assert!(picker.entry_rect(0).is_none(), "row 0 scrolled out of view");
        let first_shown = picker.entry_rows.first().map(|(i, _)| *i);
        assert!(
            first_shown.is_some_and(|i| i > 0 && picker.entry_rect(i).is_some()),
            "later rows are now visible"
        );
    }

    fn filters_hide_nonmatching_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("a.png"), b"x").unwrap();
        fs::write(root.join("b.txt"), b"x").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        let picker = FilePicker::new("Open", &root, vec![root.clone()])
            .with_filters(vec![
                FileFilter::new("Images", &["png", "jpg"]),
                FileFilter::all(),
            ])
            .settled();
        // Directory + the png survive; the txt is filtered out.
        let names: Vec<_> = picker.entries().iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"sub".to_string()));
        assert!(names.contains(&"a.png".to_string()));
        assert!(!names.contains(&"b.txt".to_string()));
        assert_eq!(picker.active_filter_label(), "Images");
        assert!(
            !picker.shows_filter(),
            "one type is applied without a menu to pick from"
        );
    }

    fn cycle_filter_switches_listing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("a.png"), b"x").unwrap();
        fs::write(root.join("b.txt"), b"x").unwrap();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()])
            .with_filters(vec![
                FileFilter::new("Images", &["png"]),
                FileFilter::new("Documents", &["txt"]),
            ])
            .settled();
        assert_eq!(picker.entries().len(), 1); // just the png
        picker.cycle_filter();
        settle(&mut picker);
        assert_eq!(picker.active_filter_label(), "Documents");
        assert_eq!(picker.entries().len(), 1); // just the txt
        picker.cycle_filter();
        settle(&mut picker);
        assert_eq!(picker.active_filter_label(), "All files");
        assert_eq!(picker.entries().len(), 2); // png + txt
    }

    /// Writes native-size and accessibility-state GPU renders for the manual visual audit. Kept
    /// ignored so ordinary unit runs remain hermetic; run with
    /// `cargo test -p ui file_picker_visual_audit -- --ignored --nocapture`.
    #[ignore = "developer visual audit writes /tmp/tilo-file-picker-focus"]
    fn file_picker_visual_audit() {
        use vello::peniko::Color;

        let (_guard, root) = temp_tree();
        let output = Path::new("/tmp/tilo-file-picker-focus");
        fs::create_dir_all(output).expect("visual-audit output directory");
        for (name, width, height, mode, theme) in [
            (
                "open-800x600",
                800,
                600,
                FilePickerMode::Open,
                Theme::dark(),
            ),
            (
                "save-800x600",
                800,
                600,
                FilePickerMode::Save,
                Theme::dark(),
            ),
            (
                "open-1280x800",
                1280,
                800,
                FilePickerMode::Open,
                Theme::dark(),
            ),
            (
                "save-1280x800",
                1280,
                800,
                FilePickerMode::Save,
                Theme::dark(),
            ),
            (
                "open-800x600-large-text",
                800,
                600,
                FilePickerMode::Open,
                Theme::dark().with_text_scale(1.5),
            ),
            (
                "save-800x600-large-text",
                800,
                600,
                FilePickerMode::Save,
                Theme::dark().with_text_scale(1.5),
            ),
            (
                "open-800x600-high-contrast",
                800,
                600,
                FilePickerMode::Open,
                Theme::high_contrast(),
            ),
            (
                "save-800x600-high-contrast",
                800,
                600,
                FilePickerMode::Save,
                Theme::high_contrast(),
            ),
        ] {
            let mut picker = FilePicker::new("Choose a file", &root, vec![root.clone()])
                .with_mode(mode)
                .settled();
            let pixel_width = width;
            let pixel_height = height;
            let width = f64::from(pixel_width);
            let height = f64::from(pixel_height);
            picker.set_rect(Rect::new(
                width * 0.19,
                height * 0.14,
                width * 0.81,
                height * 0.86,
            ));
            Widget::set_focus(&mut picker, true);
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            picker.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::default(),
            );
            let mut renderer = crate::offscreen::OffscreenRenderer::new(pixel_width, pixel_height);
            renderer.render_png(
                &scene,
                Color::from_rgba8(20, 24, 32, 255),
                output.join(format!("{name}.png")),
            );
        }
    }

    scrolling_windows_the_visible_rows();
    filters_hide_nonmatching_files();
    cycle_filter_switches_listing();
    file_picker_visual_audit();
}

#[test]
fn keyboard_focus_cycles_selects_and_types_ahead() {
    fn tab_and_shift_tab_cycle_only_enabled_modal_controls() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));

        // The Open action is disabled with no selected file, so focus wraps
        // List → Cancel → List instead of landing on a destructive-looking
        // button that cannot act.
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Cancel));
        Dispatch::new()
            .modifiers(shift())
            .send(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::List));

        picker.set_selection(1);
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Cancel));
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Confirm));
    }

    fn list_arrows_select_rows_and_enter_accepts_the_selected_file() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::ArrowDown),
        );
        settle(&mut picker);
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::ArrowDown),
        );
        settle(&mut picker);
        assert_eq!(picker.selection(), Some(1), "second row is alpha.txt");
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
        settle(&mut picker);
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
        );
    }

    fn keyboard_down_then_enter_enters_directory() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        // ArrowDown selects entry 0 (the `nested` directory); Enter navigates.
        dispatch(
            &mut picker,
            &key_named(winit::keyboard::NamedKey::ArrowDown),
        );
        settle(&mut picker);
        assert_eq!(picker.selection(), Some(0));
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
        settle(&mut picker);
        assert_eq!(picker.current_dir(), root.join("nested").as_path());
    }

    fn type_ahead_jumps_to_matching_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("apple.txt"), b"x").unwrap();
        fs::write(root.join("banana.txt"), b"x").unwrap();
        fs::write(root.join("cherry.txt"), b"x").unwrap();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        dispatch(&mut picker, &key_char("c")); // jump to cherry.txt
        settle(&mut picker);
        let sel = picker.selection().unwrap();
        assert_eq!(picker.entries()[sel].name, "cherry.txt");
    }

    tab_and_shift_tab_cycle_only_enabled_modal_controls();
    list_arrows_select_rows_and_enter_accepts_the_selected_file();
    keyboard_down_then_enter_enters_directory();
    type_ahead_jumps_to_matching_entry();
}

#[test]
fn the_redesigned_dialog_names_selects_and_navigates_by_token_sized_parts() {
    fn save_preselects_the_name_without_its_extension() {
        let (_g, root) = temp_tree();
        let picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .with_filename("report.final.pdf")
            .settled();
        assert_eq!(
            picker.filename.selection_range_public(),
            Some((0, "report.final".len())),
            "typing replaces the name and keeps the type"
        );
        let hidden = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .with_filename(".bashrc")
            .settled();
        assert_eq!(
            hidden.filename.selection_range_public(),
            Some((0, ".bashrc".len())),
            "a dotfile has no extension to keep"
        );
    }

    fn the_sidebar_lists_allowed_roots_and_a_click_enters_one() {
        let (_g, root) = temp_tree();
        let inner = root.join("docs");
        fs::create_dir(&inner).unwrap();
        let mut picker =
            FilePicker::new("Open", &root, vec![root.clone(), inner.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
        assert!(picker.has_sidebar());
        let at = picker
            .places
            .iter()
            .position(|p| p.path == inner)
            .expect("an accessible root is a place");
        let rect = picker.place_rects[at];
        dispatch(&mut picker, &cursor_at(rect.center()));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.current_dir(), inner);
        assert_eq!(picker.active_place(), Some(at));

        let mut narrow = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        narrow.set_rect(Rect::new(0.0, 0.0, 420.0, 420.0));
        assert!(!narrow.has_sidebar(), "a narrow dialog keeps the list");
    }

    fn the_type_menu_opens_picks_with_the_keyboard_and_closes() {
        use winit::keyboard::NamedKey;
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()])
            .with_filters(vec![
                FileFilter::new("Images", &["png"]),
                FileFilter::new("Documents", &["txt"]),
            ])
            .settled();
        picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
        Widget::set_focus(&mut picker, true);
        picker.focus_control(PickerFocus::Filter);
        let expanded = |picker: &FilePicker| {
            Widget::accessibility(picker)
                .expect("picker accessibility")
                .children
                .iter()
                .find(|node| node.key == Some(PickerFocus::Filter.id()))
                .expect("file type control")
                .state
                .expanded
        };
        assert_eq!(expanded(&picker), Some(false));
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert!(picker.filter_open);
        assert_eq!(expanded(&picker), Some(true));
        dispatch(&mut picker, &key_named(NamedKey::ArrowDown));
        settle(&mut picker);
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert!(!picker.filter_open);
        assert_eq!(expanded(&picker), Some(false));
        assert_eq!(picker.active_filter_label(), "Documents");
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        dispatch(&mut picker, &key_named(NamedKey::Escape));
        settle(&mut picker);
        assert!(
            !picker.filter_open && picker.outcome().is_none(),
            "Escape closes the menu, not the dialog"
        );
    }

    fn a_deep_path_keeps_the_current_folder_inside_the_path_bar() {
        let (_g, root) = temp_tree();
        let mut deep = root.clone();
        for level in 0..9 {
            deep.push(format!("a-rather-long-folder-name-{level}"));
        }
        fs::create_dir_all(&deep).unwrap();
        let mut picker = FilePicker::new("Open", &deep, vec![root.clone()]).settled();
        picker.set_rect(Rect::new(0.0, 0.0, 560.0, 420.0));
        picker.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        let bar = picker.path_rect();
        let painted = picker.crumbs.segment_rects();
        let (last, _) = *painted.last().expect("crumbs");
        assert_eq!(
            picker.crumb_targets[last], deep,
            "the current folder always shows"
        );
        assert!(painted.len() < 10, "leading segments drop out");
        assert!(painted.iter().all(|(_, r)| r.x1 <= bar.x1 + 1e-6));
    }

    save_preselects_the_name_without_its_extension();
    the_sidebar_lists_allowed_roots_and_a_click_enters_one();
    the_type_menu_opens_picks_with_the_keyboard_and_closes();
    a_deep_path_keeps_the_current_folder_inside_the_path_bar();
}

fn paint(picker: &mut FilePicker) {
    picker.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::default(),
    );
}

/// Wait for everything the picker asked its worker (a listing, a check
/// before confirming, the usual folders) to come back, and take it, as the
/// host does when the worker wakes it.
fn settle(picker: &mut FilePicker) {
    tilo_test_env::wait_until("the chooser's worker to answer", || {
        picker.poll_lookups();
        !picker.reading()
    });
}

/// A picker once its first listing is in.
trait Settled {
    fn settled(self) -> Self;
}

impl Settled for FilePicker {
    fn settled(mut self) -> Self {
        settle(&mut self);
        self
    }
}

/// Counts how often the picker wakes its host.
struct CountingWake(std::sync::atomic::AtomicUsize);

impl std::task::Wake for CountingWake {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn names(picker: &FilePicker) -> Vec<String> {
    picker.entries().iter().map(|e| e.name.clone()).collect()
}

fn state_of(picker: &FilePicker) -> String {
    picker.semantic_inspection(0).expect("inspection")["state"]
        .as_str()
        .expect("state")
        .to_owned()
}

fn totp() -> Option<Requester> {
    Some(Requester {
        name: "TOTP".to_owned(),
        glyph: Icon::Lock,
        image: None,
    })
}

#[test]
fn the_sheet_says_who_asks_and_finds_files_from_the_path_bar_and_search() {
    fn the_heading_names_the_app_or_gives_its_purpose() {
        let (_g, root) = temp_tree();
        let asking = FilePicker::new("", &root, vec![root.clone()])
            .with_multiple(false)
            .with_requester(totp())
            .settled();
        let heading = asking.heading();
        assert!(
            heading.contains("TOTP") && heading.contains("wants to open a file"),
            "{heading}"
        );
        assert!(!asking.has_subtitle(), "the heading already names the app");
        assert_eq!(
            Widget::accessibility(&asking)
                .expect("dialog")
                .name
                .as_deref(),
            Some(heading.as_str())
        );
        let purpose = FilePicker::new("Choose a QR code image", &root, vec![root.clone()])
            .with_requester(totp())
            .settled();
        assert_eq!(purpose.heading(), "Choose a QR code image");
        assert!(
            purpose.has_subtitle(),
            "the app's name sits under its purpose"
        );
        let saving = FilePicker::new("", &root, vec![root.clone()])
            .with_requester(totp())
            .with_mode(FilePickerMode::Save)
            .settled();
        assert!(saving.heading().contains("wants to save a file"));
        assert_eq!(
            saving.confirm_label(),
            "Save",
            "the button stays the outcome"
        );
    }

    fn the_type_menu_shows_only_when_the_app_named_more_than_one_type() {
        let (_g, root) = temp_tree();
        let plain = FilePicker::new("", &root, vec![root.clone()])
            .with_filters(Vec::new())
            .settled();
        assert!(!plain.shows_filter());
        assert_eq!(plain.active_filter_label(), "All files");
        let two = FilePicker::new("", &root, vec![root.clone()])
            .with_filters(vec![
                FileFilter::new("Images", &["png"]),
                FileFilter::new("Documents", &["txt"]),
                FileFilter::all(),
            ])
            .settled();
        assert!(two.shows_filter());
        let labels: Vec<_> = two.filters.iter().map(|f| f.label.clone()).collect();
        assert_eq!(labels, ["Images", "Documents", "All files"]);
    }

    fn recent_leads_the_sidebar_and_lists_what_was_chosen() {
        let (_g, root) = temp_tree();
        let none = FilePicker::new("", &root, vec![root.clone()]).settled();
        assert!(none.places.iter().all(|p| !p.recent), "nothing chosen yet");
        let chosen = root.join("nested").join("gamma.txt");
        let mut picker = FilePicker::new("", &root, vec![root.clone()])
            .with_home(Some(root.clone()))
            .with_recent(vec![chosen.clone(), PathBuf::from("/etc/passwd")])
            .settled();
        assert!(picker.places[0].recent, "Recent is the first place");
        picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
        let rect = picker.place_rects[0];
        dispatch(&mut picker, &cursor_at(rect.center()));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        settle(&mut picker);
        assert_eq!(
            names(&picker),
            ["gamma.txt"],
            "only what the roots allow is listed"
        );
        assert_eq!(picker.crumbs.segments(), ["Recent"]);
        assert!(!picker.can_go_up());
    }

    fn typing_narrows_the_folder_and_enter_searches_every_place() {
        use winit::keyboard::NamedKey;
        let (_g, root) = temp_tree();
        let gamma = root.join("nested").join("gamma.txt");
        let found = vec![gamma.clone(), PathBuf::from("/etc/passwd")];
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        let search: PickerSearch = Arc::new(move |query: &str| {
            if let Ok(mut seen) = seen.lock() {
                seen.push((query.to_owned(), std::thread::current().id()));
            }
            found.clone()
        });
        let woken = Arc::new(CountingWake(std::sync::atomic::AtomicUsize::new(0)));
        let mut picker = FilePicker::new("", &root, vec![root.clone()])
            .with_search(Some(search))
            .with_waker(Some(std::task::Waker::from(woken.clone())))
            .settled();
        picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
        Widget::set_focus(&mut picker, true);
        picker.focus_control(PickerFocus::List);
        dispatch(&mut picker, &key_char("b"));
        settle(&mut picker);
        assert_eq!(
            picker.focus,
            Some(PickerFocus::Search),
            "typing goes to search"
        );
        assert_eq!(
            names(&picker),
            ["beta.txt"],
            "the folder narrows as you type"
        );
        dispatch(&mut picker, &key_named(NamedKey::Escape));
        settle(&mut picker);
        assert_eq!(names(&picker).len(), 3, "Escape empties the search");
        assert!(picker.outcome().is_none(), "and keeps the dialog");

        let listed_wakes = woken.0.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            listed_wakes, 1,
            "the host is woken when the folder is listed"
        );
        picker.search_for("gamma");
        assert!(picker.looking_up(), "the search runs elsewhere");
        settle(&mut picker);
        let asked = asked.lock().map(|asked| asked.clone()).unwrap_or_default();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, "gamma");
        assert_ne!(
            asked[0].1,
            std::thread::current().id(),
            "the index is never read on the UI thread"
        );
        assert_eq!(
            woken.0.load(std::sync::atomic::Ordering::SeqCst),
            listed_wakes.saturating_add(1),
            "the host is woken when the results are in"
        );
        assert_eq!(
            picker
                .entries()
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>(),
            [gamma],
            "results outside the roots are left out"
        );
        picker.set_selection(0);
        picker.focus_control(PickerFocus::List);
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert!(
            matches!(picker.outcome(), Some(FilePickerOutcome::Selected(paths)) if paths.len() == 1),
            "a found file is chosen like any other"
        );
    }

    fn search_and_recent_are_picker_operations() {
        let (_g, root) = temp_tree();
        let gamma = root.join("nested").join("gamma.txt");
        let mut picker = FilePicker::new("", &root, vec![root.clone()]).settled();
        let state = state_of(&picker);
        assert!(
            picker
                .perform_semantic(Some(&state), semantic::PickerOperation::Recent)
                .is_err(),
            "nothing was chosen here yet"
        );
        settle(&mut picker);
        let state = state_of(&picker);
        let seen = picker
            .perform_semantic(
                Some(&state),
                semantic::PickerOperation::Search {
                    query: "alp".to_owned(),
                },
            )
            .expect("search");
        settle(&mut picker);
        assert_eq!(seen["query"], "alp");
        assert_eq!(names(&picker), ["alpha.txt"]);

        let mut recent = FilePicker::new("", &root, vec![root.clone()])
            .with_recent(vec![gamma])
            .settled();
        let state = state_of(&recent);
        let seen = recent
            .perform_semantic(Some(&state), semantic::PickerOperation::Recent)
            .expect("recent");
        settle(&mut recent);
        assert_eq!(seen["listing"], "recent");
        assert_eq!(seen["looking_up"], true, "an agent inspects again");
        settle(&mut recent);
        assert_eq!(names(&recent), ["gamma.txt"]);
    }

    fn the_path_bar_starts_at_home_goes_up_on_a_click_and_takes_a_typed_path() {
        use winit::keyboard::NamedKey;
        let (_g, root) = temp_tree();
        let nested = root.join("nested");
        let mut picker = FilePicker::new("", &nested, vec![root.clone()])
            .with_home(Some(root.clone()))
            .settled();
        assert_eq!(picker.crumbs.segments(), ["Home", "nested"]);
        picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
        paint(&mut picker);
        let (_, home) = picker.crumbs.segment_rects()[0];
        dispatch(&mut picker, &cursor_at(home.center()));
        settle(&mut picker);
        dispatch(&mut picker, &mouse_press());
        settle(&mut picker);
        assert_eq!(picker.current_dir(), root, "a segment goes up to it");

        Widget::set_focus(&mut picker, true);
        Dispatch::new()
            .modifiers(ctrl())
            .send(&mut picker, &key_char("l"));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::Location));
        assert_eq!(picker.location.as_ref().map(|f| f.value()), Some("~/"));
        dispatch(&mut picker, &key_char("~/nested/gamma.txt"));
        settle(&mut picker);
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert!(picker.location.is_none(), "the trail comes back");
        assert_eq!(picker.current_dir(), nested);
        assert_eq!(picker.selected_paths(), [nested.join("gamma.txt")]);

        picker.edit_location();
        dispatch(&mut picker, &key_char("/"));
        settle(&mut picker);
        dispatch(&mut picker, &key_named(NamedKey::Enter));
        settle(&mut picker);
        assert!(picker.location_invalid, "outside the roots stays marked");
        assert_eq!(picker.current_dir(), nested);
        dispatch(&mut picker, &key_named(NamedKey::Escape));
        settle(&mut picker);
        assert!(picker.location.is_none() && picker.outcome().is_none());
    }

    the_heading_names_the_app_or_gives_its_purpose();
    the_type_menu_shows_only_when_the_app_named_more_than_one_type();
    recent_leads_the_sidebar_and_lists_what_was_chosen();
    typing_narrows_the_folder_and_enter_searches_every_place();
    search_and_recent_are_picker_operations();
    the_path_bar_starts_at_home_goes_up_on_a_click_and_takes_a_typed_path();
}

/// A session with no home hands the picker a temporary folder to start in
/// and roots that do not hold it. The picker must open where it may save,
/// so confirming a Save answers instead of refusing every name.
#[test]
fn a_save_picker_started_outside_its_roots_opens_at_a_root_and_saves_there() {
    let (_g, root) = temp_tree();
    let elsewhere = tempfile::tempdir().expect("a folder outside the roots");
    let mut picker = FilePicker::new("Save", elsewhere.path(), vec![root.clone()])
        .with_mode(FilePickerMode::Save)
        .with_filename("backup.tilo-identity")
        .settled();
    assert_eq!(picker.current_dir(), root.as_path());
    picker.click_open();
    settle(&mut picker);
    assert_eq!(picker.name_error(), None);
    assert_eq!(
        picker.take_outcome(),
        Some(FilePickerOutcome::Selected(vec![
            root.join("backup.tilo-identity")
        ]))
    );
}
