use super::*;
use crate::testing::{Dispatch, dispatch};
use automerge::AutoCommit;
use std::fs;
use winit::event::ElementState;

fn cursor_at(p: Point) -> WindowEvent {
    crate::synthetic_input::cursor_moved(p.x, p.y)
}

use crate::synthetic_input::mouse_press;

fn temp_tree() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    fs::write(root.join("alpha.txt"), b"a").unwrap();
    fs::write(root.join("beta.txt"), b"b").unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("nested").join("gamma.txt"), b"c").unwrap();
    (dir, root)
}

tilo_test_env::scenario! {
    fn the_picker_opens_navigates_and_resolves;

    phase picker_opens_as_modal_and_resolves_open_or_cancel;
    {
            use crate::a11y::Role;
            let (_g, root) = temp_tree();
            let picker = FilePicker::new("Open file", &root, vec![root.clone()]).settled();
            let node = Widget::accessibility(&picker).expect("file picker is a dialog");
            assert_eq!(node.role, Role::Dialog);
            assert_eq!(node.name.as_deref(), Some("Open file"));
            assert!(node.state.modal, "the file chooser traps focus");

            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
            assert_eq!(picker.entries().len(), 3);
            assert!(picker.entries()[0].is_dir);
            assert_eq!(picker.entries()[0].name, "nested");
            let target = root.join("alpha.txt");
            assert!(picker.select_by_path(&target));
            picker.click_open();
            settle(&mut picker);
            assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Selected(vec![target])));
            assert!(picker.take_outcome().is_none(), "the outcome drains once");

            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
            picker.click_cancel();
            assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
            assert!(picker.take_outcome().is_none(), "the outcome drains once");

            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Escape));
            assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));

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

    phase navigation_stays_under_roots;
    {
        let (_g, root) = temp_tree();
        let nested = root.join("nested");
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]).settled();
        picker.navigate_to(&nested);
        settle(&mut picker);
        assert_eq!(picker.current_dir(), nested.as_path());
        assert_eq!(picker.entries().len(), 1);
        assert_eq!(picker.entries()[0].name, "gamma.txt");
        assert!(picker.can_navigate_to(&root));
        assert!(picker.can_navigate_to(&nested));
        assert!(!picker.can_navigate_to(Path::new("/etc")));
        // The up button walks back to the root.
        assert!(picker.can_go_up());
        picker.go_up();
        settle(&mut picker);
        assert_eq!(picker.current_dir(), root.as_path());
        // A folder outside every root is silently dropped.
        let cwd_before = picker.current_dir().to_path_buf();
        picker.navigate_to(Path::new("/etc"));
        settle(&mut picker);
        assert_eq!(picker.current_dir(), cwd_before.as_path());
    }

    phase pointer_rows_and_footer_buttons_navigate_select_and_resolve;
    {
        // A click on a folder row walks into it; a click on a file row selects
        // it; a double click on the file opens it.
        let (_g, root) = temp_tree();
        let mut picker = opened(&root);
        let center = picker.entry_rect(0).expect("row 0").center();
        feed(&mut picker, &cursor_at(center));
        feed(&mut picker, &mouse_press());
        assert_eq!(picker.current_dir(), root.join("nested").as_path());
        picker.go_up();
        settle(&mut picker);
        let center = picker.entry_rect(1).expect("row 1").center();
        feed(&mut picker, &cursor_at(center));
        feed(&mut picker, &mouse_press());
        assert_eq!(picker.selected_indices(), vec![1]);
        feed(&mut picker, &mouse_press());
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")])),
            "a second click on the file opens it"
        );

        // The footer's Cancel resolves Cancelled; its Open resolves the
        // selection.
        let mut picker = opened(&root);
        let center = picker.cancel_rect().center();
        feed(&mut picker, &cursor_at(center));
        feed(&mut picker, &mouse_press());
        assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));

        let mut picker = opened(&root);
        let target = root.join("alpha.txt");
        picker.select_by_path(&target);
        let center = picker.open_rect().center();
        feed(&mut picker, &cursor_at(center));
        feed(&mut picker, &mouse_press());
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![target]))
        );

        // Ctrl-click adds a second file, and Open hands both over.
        let mut picker = opened(&root);
        let r1 = picker.entry_rect(1).unwrap().center();
        feed(&mut picker, &cursor_at(r1));
        Dispatch::new().modifiers(ctrl()).send(&mut picker, &mouse_press());
        settle(&mut picker);
        let r2 = picker.entry_rect(2).unwrap().center();
        feed(&mut picker, &cursor_at(r2));
        Dispatch::new().modifiers(ctrl()).send(&mut picker, &mouse_press());
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

    phase keyboard_focus_cycles_selects_and_types_ahead;
    {
        // Tab and Shift+Tab cycle only the enabled modal controls: the Open
        // action is disabled with no selected file, so focus wraps
        // List → Cancel → List instead of landing on a destructive-looking
        // button that cannot act.
        let (_g, root) = temp_tree();
        let mut picker = opened(&root);
        feed(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        assert_eq!(picker.focus, Some(PickerFocus::Cancel));
        Dispatch::new()
            .modifiers(shift())
            .send(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        settle(&mut picker);
        assert_eq!(picker.focus, Some(PickerFocus::List));
        picker.set_selection(1);
        feed(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        assert_eq!(picker.focus, Some(PickerFocus::Cancel));
        feed(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
        assert_eq!(picker.focus, Some(PickerFocus::Confirm));

        // The list's arrows select a file and Enter accepts it; down onto the
        // folder row and Enter walks into it.
        let mut picker = opened(&root);
        feed(&mut picker, &key_named(winit::keyboard::NamedKey::ArrowDown));
        feed(&mut picker, &key_named(winit::keyboard::NamedKey::ArrowDown));
        assert_eq!(picker.selected_indices(), vec![1], "second row is alpha.txt");
        feed(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
        assert_eq!(
            picker.take_outcome(),
            Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
        );
        let mut picker = opened(&root);
        feed(&mut picker, &key_named(winit::keyboard::NamedKey::ArrowDown));
        assert_eq!(picker.selected_indices(), vec![0]);
        feed(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
        assert_eq!(picker.current_dir(), root.join("nested").as_path());

        // Typing a letter jumps to the first entry that starts with it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("apple.txt"), b"x").unwrap();
        fs::write(root.join("banana.txt"), b"x").unwrap();
        fs::write(root.join("cherry.txt"), b"x").unwrap();
        let mut picker = opened(&root);
        feed(&mut picker, &key_char("c"));
        let sel = picker.selected_indices()[0];
        assert_eq!(picker.entries()[sel].name, "cherry.txt");
    }


    phase the_sheet_says_who_asks_and_finds_files_from_the_path_bar_and_search;
    {
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
            feed(&mut picker, &cursor_at(rect.center()));
            feed(&mut picker, &mouse_press());
            settle(&mut picker);
            assert_eq!(
                names(&picker),
                ["gamma.txt"],
                "only what the roots allow is listed"
            );
            assert_eq!(picker.crumbs.segments(), ["Recent"]);
            assert!(!picker.can_go_up());

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
            feed(&mut picker, &key_char("b"));
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
            feed(&mut picker, &key_named(NamedKey::Escape));
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
            feed(&mut picker, &key_named(NamedKey::Enter));
            assert!(
                matches!(picker.outcome(), Some(FilePickerOutcome::Selected(paths)) if paths.len() == 1),
                "a found file is chosen like any other"
            );

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

            let (_g, root) = temp_tree();
            let alpha = semantic::entry_id(&root.join("alpha.txt"));
            let beta = semantic::entry_id(&root.join("beta.txt"));
            let mut single = FilePicker::new("", &root, vec![root.clone()])
                .with_multiple(false)
                .settled();
            for entry in [alpha.clone(), beta.clone()] {
                let state = state_of(&single);
                single
                    .perform_semantic(
                        Some(&state),
                        semantic::PickerOperation::AlsoSelect { entry },
                    )
                    .expect("select");
            }
            assert_eq!(
                single.selected_indices().len(),
                1,
                "one file where one is taken"
            );

            let mut several = FilePicker::new("", &root, vec![root.clone()])
                .with_multiple(true)
                .settled();
            for entry in [alpha, beta] {
                let state = state_of(&several);
                several
                    .perform_semantic(
                        Some(&state),
                        semantic::PickerOperation::AlsoSelect { entry },
                    )
                    .expect("select");
            }
            assert_eq!(several.selected_indices().len(), 2);
            assert!(several.can_confirm());

            let mut typed = FilePicker::new("", &root, vec![root.clone()])
                .with_filters(vec![
                    FileFilter::new("Text", &["txt"]),
                    FileFilter::new("Markdown", &["md"]),
                ])
                .settled();
            assert_eq!(typed.filter_labels().len(), 3);
            let state = state_of(&typed);
            typed
                .perform_semantic(Some(&state), semantic::PickerOperation::Filter { index: 1 })
                .expect("filter");
            assert_eq!(typed.active_filter_label(), "Markdown");
            let state = state_of(&typed);
            assert!(
                typed
                    .perform_semantic(Some(&state), semantic::PickerOperation::Filter { index: 3 })
                    .is_err()
            );

            let (_g, root) = temp_tree();
            let nested = root.join("nested");
            let mut picker = FilePicker::new("", &nested, vec![root.clone()])
                .with_home(Some(root.clone()))
                .settled();
            assert_eq!(picker.crumbs.segments(), ["Home", "nested"]);
            picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
            paint(&mut picker);
            let (_, home) = picker.crumbs.segment_rects()[0];
            feed(&mut picker, &cursor_at(home.center()));
            feed(&mut picker, &mouse_press());
            assert_eq!(picker.current_dir(), root, "a segment goes up to it");

            Widget::set_focus(&mut picker, true);
            Dispatch::new()
                .modifiers(ctrl())
                .send(&mut picker, &key_char("l"));
            settle(&mut picker);
            assert_eq!(picker.focus, Some(PickerFocus::Location));
            assert_eq!(picker.location.as_ref().map(|f| f.value()), Some("~/"));
            feed(&mut picker, &key_char("~/nested/gamma.txt"));
            feed(&mut picker, &key_named(NamedKey::Enter));
            assert!(picker.location.is_none(), "the trail comes back");
            assert_eq!(picker.current_dir(), nested);
            assert_eq!(picker.selected_paths(), [nested.join("gamma.txt")]);

            picker.edit_location();
            feed(&mut picker, &key_char("/"));
            feed(&mut picker, &key_named(NamedKey::Enter));
            assert!(picker.location_invalid, "outside the roots stays marked");
            assert_eq!(picker.current_dir(), nested);
            feed(&mut picker, &key_named(NamedKey::Escape));
            assert!(picker.location.is_none() && picker.outcome().is_none());
    }
}

tilo_test_env::scenario! {
    fn directory_mode_chooses_a_folder;

    phase directory_mode_lists_selects_and_chooses_folders;
    {
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

            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let first = root.join("a");
            let second = root.join("b");
            fs::create_dir(&first).unwrap();
            fs::create_dir(&second).unwrap();

            let mut single = choosing(&root, false);
            for index in [0, 1] {
                let center = single.entry_rect(index).unwrap().center();
                feed(&mut single, &cursor_at(center));
                Dispatch::new()
                    .modifiers(ctrl())
                    .send(&mut single, &mouse_press());
                settle(&mut single);
            }
            assert_eq!(single.selected_indices(), vec![1]);

            let mut multiple = choosing(&root, true);
            for index in [0, 1] {
                let center = multiple.entry_rect(index).unwrap().center();
                feed(&mut multiple, &cursor_at(center));
                Dispatch::new()
                    .modifiers(ctrl())
                    .send(&mut multiple, &mouse_press());
                settle(&mut multiple);
            }
            assert_eq!(multiple.selected_indices(), vec![0, 1]);

            let mut keyboard_single = choosing(&root, false);
            feed(&mut keyboard_single, &key_named(winit::keyboard::NamedKey::ArrowDown));
            Dispatch::new().modifiers(shift()).send(
                &mut keyboard_single,
                &key_named(winit::keyboard::NamedKey::ArrowDown),
            );
            settle(&mut keyboard_single);
            assert_eq!(keyboard_single.selected_indices(), vec![1]);

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

    phase directory_mode_pointer_and_keyboard_confirmation;
    {
            let (_guard, root) = temp_tree();
            let child = root.join("nested");
            let mut picker = choosing(&root, true);
            let center = picker.entry_rect(0).unwrap().center();
            feed(&mut picker, &cursor_at(center));

            feed(&mut picker, &mouse_press());
            assert_eq!(picker.current_dir(), root.as_path());
            assert_eq!(picker.selected_indices(), vec![0]);

            feed(&mut picker, &mouse_press());
            assert_eq!(picker.current_dir(), child.as_path());

            let (_guard, root) = temp_tree();
            let child = root.join("nested");
            let mut picker = choosing(&root, true);

            let row = picker.entry_rect(0).unwrap().center();
            feed(&mut picker, &cursor_at(row));
            feed(&mut picker, &mouse_press());
            let confirm = picker.open_rect().center();
            feed(&mut picker, &cursor_at(confirm));
            feed(&mut picker, &mouse_press());

            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![child]))
            );

            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let apple = root.join("apple");
            let cherry = root.join("cherry");
            fs::create_dir(&apple).unwrap();
            fs::create_dir(&cherry).unwrap();
            let mut picker = choosing(&root, true);

            feed(&mut picker, &key_char("c"));
            assert_eq!(picker.selected_paths(), std::slice::from_ref(&cherry));
            assert_eq!(picker.focus, Some(PickerFocus::Search));
            for _ in 0..3 {
                feed(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            }
            assert_eq!(picker.focus, Some(PickerFocus::Confirm));
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));

            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![cherry]))
            );

            let mut navigate = choosing(&root, true);
            feed(&mut navigate, &key_named(winit::keyboard::NamedKey::ArrowDown));
            feed(&mut navigate, &key_named(winit::keyboard::NamedKey::Enter));
            assert_eq!(navigate.current_dir(), apple.as_path());

            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory)
                .settled();

            assert_eq!(picker.focus, Some(PickerFocus::Search));
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Cancel));
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Confirm));
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.clone()]))
            );

            let mut cancelled = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory)
                .settled();
            feed(&mut cancelled, &key_named(winit::keyboard::NamedKey::Escape));
            assert_eq!(cancelled.take_outcome(), Some(FilePickerOutcome::Cancelled));
    }

    phase directory_confirmation_revalidates_the_filesystem;
    {
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

        #[cfg(unix)]
        {
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
        {
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
    }
}

tilo_test_env::scenario! {
    fn save_mode_names_a_file_under_its_roots;

    phase save_mode_filename_resolves_and_rejects_escapes;
    {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Save)
                .settled();
            for ch in ["n", "e", "w"] {
                feed(&mut picker, &key_char(ch));
            }
            feed(&mut picker, &key_char("."));
            for ch in ["t", "x", "t"] {
                feed(&mut picker, &key_char(ch));
            }
            assert_eq!(picker.filename(), "new.txt");
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Backspace));
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Backspace));
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Backspace));
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Backspace));
            feed(&mut picker, &key_char("👍🏽"));
            feed(&mut picker, &key_char("e\u{301}"));
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Backspace));
            assert_eq!(picker.filename(), "new👍🏽");
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Backspace));
            assert_eq!(picker.filename(), "new");
            feed(&mut picker, &WindowEvent::Ime(winit::event::Ime::Preedit("tx".into(), Some((2, 2)))));
            assert_eq!(picker.filename(), "new", "a composition is not the value");
            feed(&mut picker, &WindowEvent::Ime(winit::event::Ime::Commit(".txt".into())));
            assert_eq!(picker.filename(), "new.txt");
            picker.click_open();
            settle(&mut picker);
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("new.txt")]))
            );

            use crate::a11y::Role;
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Save)
                .settled();
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            Widget::set_focus(&mut picker, true);
            assert!(crate::ime::ImeWindowState::from_widget(&picker).is_some());
            let original = picker.ime_edit_fingerprint(&[7; 32]).unwrap();
            feed(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Cancel));
            assert!(crate::ime::ImeWindowState::from_widget(&picker).is_none());
            Dispatch::new()
                .modifiers(shift())
                .send(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            settle(&mut picker);
            assert_eq!(picker.focus, Some(PickerFocus::Filename));
            feed(&mut picker, &key_char("x"));
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
            let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Save)
                .with_filename("nested/")
                .settled();
            picker.click_open();
            settle(&mut picker);
            assert_eq!(picker.take_outcome(), None);
            assert_eq!(picker.current_dir(), root.join("nested").as_path());
            assert_eq!(picker.filename(), "");
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
            feed(&mut picker, &key_char("x"));
            assert_eq!(picker.name_error(), None);

        #[cfg(unix)]
        {
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
    }

    phase saving_over_an_existing_file_asks_first;
    {
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

            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "alpha.txt");
            feed(&mut picker, &key_named(NamedKey::Enter));
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
            feed(&mut picker, &key_char("x"));
            assert_eq!(picker.filename(), "alpha.txt");

            feed(&mut picker, &key_named(NamedKey::Escape));
            assert_eq!(picker.replace_prompt(), None);
            assert_eq!(picker.take_outcome(), None, "Escape leaves the picker open");
            assert_eq!(picker.focus, Some(PickerFocus::Filename));
            assert_eq!(fs::read(root.join("alpha.txt")).unwrap(), b"a");

            feed(&mut picker, &key_named(NamedKey::Enter));
            assert!(picker.replace_prompt().is_some());
            feed(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(
                picker.replace_prompt(),
                None,
                "Enter on Cancel keeps the file"
            );
            assert_eq!(picker.take_outcome(), None);

            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "alpha.txt");
            feed(&mut picker, &key_named(NamedKey::Enter));
            feed(&mut picker, &key_named(NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Replace));
            Dispatch::new()
                .modifiers(shift())
                .send(&mut picker, &key_named(NamedKey::Tab));
            settle(&mut picker);
            assert_eq!(picker.focus, Some(PickerFocus::KeepExisting));
            feed(&mut picker, &key_named(NamedKey::ArrowRight));
            assert_eq!(picker.focus, Some(PickerFocus::Replace));
            feed(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
            );

            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "alpha.txt");
            let at = picker.open_rect().center();
            feed(&mut picker, &cursor_at(at));
            feed(&mut picker, &mouse_press());
            assert!(picker.replace_prompt().is_some(), "Save asks first");
            feed(&mut picker, &mouse_press());
            assert_eq!(picker.take_outcome(), None);
            let at = picker.keep_existing_rect.center();
            feed(&mut picker, &cursor_at(at));
            feed(&mut picker, &mouse_press());
            assert_eq!(picker.replace_prompt(), None);
            assert_eq!(picker.take_outcome(), None);

            let at = picker.open_rect().center();
            feed(&mut picker, &cursor_at(at));
            feed(&mut picker, &mouse_press());
            let at = picker.replace_rect.center();
            feed(&mut picker, &cursor_at(at));
            feed(&mut picker, &mouse_press());
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
            );

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

            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "fresh.txt");
            feed(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("fresh.txt")]))
            );

            let mut picker = save_picker(&root, "nested");
            feed(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(picker.take_outcome(), None);
            assert_eq!(picker.current_dir(), root.join("nested").as_path());
            assert_eq!(picker.filename(), "");

            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "");
            let row = picker.entry_rect(1).expect("alpha.txt row").center();
            feed(&mut picker, &cursor_at(row));
            feed(&mut picker, &mouse_press());
            feed(&mut picker, &mouse_press());
            assert_eq!(picker.take_outcome(), None);
            assert_eq!(
                picker.replace_prompt(),
                Some(root.join("alpha.txt").as_path())
            );
    }

    phase the_name_field_selects_all_like_every_text_field;
    {
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
        feed(&mut picker, &key_char("x"));
        assert_eq!(
            picker.filename(),
            "x",
            "Ctrl+A selects the name, typing replaces it"
        );

        feed(&mut picker, &key_named(NamedKey::Tab));
        let field = picker.filename_rect().center();
        feed(&mut picker, &cursor_at(field));
        for _ in 0..3 {
            feed(&mut picker, &mouse_press());
            feed(&mut picker, &mouse_release());
        }
        feed(&mut picker, &key_char("y"));
        assert_eq!(
            picker.filename(),
            "y",
            "triple-click selects the whole name"
        );

        assert!(!picker.filename.spellcheck_enabled());
        assert!(!picker.filename.autocorrect_enabled());
    }

    phase a_save_picker_started_outside_its_roots_opens_at_a_root_and_saves_there;
    // A session with no home hands the picker a temporary folder to start in
    // and roots that do not hold it. The picker must open where it may save,
    // so confirming a Save answers instead of refusing every name.
    {
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

#[test]
fn listing_filters_and_scroll_window_rows() {
    fn filters_hide_nonmatching_files_and_cycle_the_listing() {
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
        let names: Vec<_> = picker.entries().iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"sub".to_string()));
        assert!(names.contains(&"a.png".to_string()));
        assert!(!names.contains(&"b.txt".to_string()));
        assert_eq!(picker.active_filter_label(), "Images");
        assert!(
            !picker.shows_filter(),
            "one type is applied without a menu to pick from"
        );

        // A folder holds no file of either type, so the cycle below moves the
        // listing between the two files alone.
        fs::remove_dir(root.join("sub")).unwrap();

        let mut picker = FilePicker::new("Open", &root, vec![root.clone()])
            .with_filters(vec![
                FileFilter::new("Images", &["png"]),
                FileFilter::new("Documents", &["txt"]),
            ])
            .settled();
        assert_eq!(picker.entries().len(), 1, "just the png");
        picker.cycle_filter();
        settle(&mut picker);
        assert_eq!(picker.active_filter_label(), "Documents");
        assert_eq!(picker.entries().len(), 1, "just the txt");
        picker.cycle_filter();
        settle(&mut picker);
        assert_eq!(picker.active_filter_label(), "All files");
        assert_eq!(picker.entries().len(), 2, "png + txt");
    }

    filters_hide_nonmatching_files_and_cycle_the_listing();
}

fn paint(picker: &mut FilePicker) {
    picker.draw(&mut crate::draw::DrawCtx::new(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        crate::presence::PresenceCtx::default().draw_env(),
    ));
}

fn settle(picker: &mut FilePicker) {
    tilo_test_env::wait_until("the chooser's worker to answer", || {
        picker.poll_lookups();
        !picker.reading()
    });
}

/// Give `picker` one event and let its worker answer before the next.
fn feed(picker: &mut FilePicker, event: &WindowEvent) {
    dispatch(picker, event);
    settle(picker);
}

/// An Open picker at the test window, its folder read.
fn opened(root: &Path) -> FilePicker {
    let mut picker = FilePicker::new("Open", root, vec![root.to_path_buf()]).settled();
    picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
    picker
}

/// A folder-choosing picker at the test window, its folder read.
fn choosing(root: &Path, multiple: bool) -> FilePicker {
    let mut picker = FilePicker::new("Choose", root, vec![root.to_path_buf()])
        .with_mode(FilePickerMode::Directory)
        .with_multiple(multiple)
        .settled();
    picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
    picker
}

trait Settled {
    fn settled(self) -> Self;
}

impl Settled for FilePicker {
    fn settled(mut self) -> Self {
        settle(&mut self);
        self
    }
}

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

/// The file type menu opens from the keyboard and chooses a filter; Escape then puts the
/// reopened menu away and leaves the dialog open with nothing chosen.
#[test]
fn escape_puts_the_file_type_menu_away_and_keeps_the_dialog() {
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
    feed(&mut picker, &key_named(NamedKey::Enter));
    assert!(picker.filter_open);
    assert_eq!(expanded(&picker), Some(true));
    feed(&mut picker, &key_named(NamedKey::ArrowDown));
    feed(&mut picker, &key_named(NamedKey::Enter));
    assert!(!picker.filter_open);
    assert_eq!(expanded(&picker), Some(false));
    assert_eq!(picker.active_filter_label(), "Documents");
    feed(&mut picker, &key_named(NamedKey::Enter));
    feed(&mut picker, &key_named(NamedKey::Escape));
    assert!(
        !picker.filter_open && picker.outcome().is_none(),
        "Escape closes the menu, not the dialog"
    );
}

/// Saving, typing replaces the name and keeps its type (a dotfile has none to keep); a press on
/// a place opens it; a narrow dialog drops the places and keeps the list.
#[test]
fn a_save_keeps_the_type_and_a_place_opens_where_it_is_pressed() {
    let (_g, root) = temp_tree();
    for (name, chosen) in [
        ("report.final.pdf", "report.final".len()),
        (".bashrc", ".bashrc".len()),
    ] {
        let picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .with_filename(name)
            .settled();
        assert_eq!(
            picker.filename.selection_range_public(),
            Some((0, chosen)),
            "{name}"
        );
    }

    let inner = root.join("docs");
    fs::create_dir(&inner).unwrap();
    let mut picker = FilePicker::new("Open", &root, vec![root.clone(), inner.clone()]).settled();
    picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
    assert!(picker.has_sidebar());
    let at = picker
        .places
        .iter()
        .position(|p| p.path == inner)
        .expect("an accessible root is a place");
    let rect = picker.place_rects[at];
    feed(&mut picker, &cursor_at(rect.center()));
    feed(&mut picker, &mouse_press());
    assert_eq!(picker.current_dir(), inner);
    assert_eq!(picker.active_place(), Some(at));

    let mut narrow = FilePicker::new("Open", &root, vec![root.clone()]).settled();
    narrow.set_rect(Rect::new(0.0, 0.0, 420.0, 420.0));
    assert!(!narrow.has_sidebar(), "a narrow dialog keeps the list");
}
