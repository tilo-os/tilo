use super::*;

#[test]
fn shared_menu_layout_matches_the_former_taffy_pass() {
    for initial_scale in [0.5, 1.0, 2.0, 4.0] {
        for viewport in [None, Some(vello::kurbo::Rect::new(0.0, 0.0, 320.0, 240.0))] {
            let make_menu = || {
                let mut menu = ContextMenu::with_items(vec![
                    MenuItem::action("Open", || {}),
                    MenuItem::separator(),
                    MenuItem::action("Rename", || {}),
                ]);
                if let Some(viewport) = viewport {
                    menu.open_at_clamped([290.0, 210.0], viewport);
                } else {
                    menu.open_at([290.0, 210.0]);
                }
                menu.set_text_scale(initial_scale);
                menu
            };
            let mut expected = make_menu();
            let mut tree = taffy::TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let node = expected.build_layout(&mut tree, &ui::Theme::dark(), &mut nodes);
            tree.compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
            expected.apply_layout(&tree, node, Point::ZERO);
            let mut actual = make_menu();
            layout_menu_widget(&mut actual);
            assert_eq!(actual.get_rect(), expected.get_rect());
            for index in 0..3 {
                assert_eq!(actual.item_rect(index), expected.item_rect(index));
            }
        }
    }
}

#[cfg(test)]
mod open_with_tests {
    use super::*;
    use crate::model::Entry;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct RecRunner {
        /// Paths handed to the system default handler (non-document opens).
        opened: Vec<PathBuf>,
        /// How the desktop answers them; `None` opens.
        refusal: Option<String>,
    }
    impl CommandRunner for RecRunner {
        fn open_default(&mut self, path: &Path) -> ui::portal::LaunchChannel {
            self.opened.push(path.to_path_buf());
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = tx.send(match self.refusal.clone() {
                Some(reason) => ui::portal::LaunchOutcome::Failed(reason),
                None => ui::portal::LaunchOutcome::Opened,
            });
            rx
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
            observed: None,
        }
    }

    #[test]
    fn files_are_classified_for_open_and_merge_by_extension() {
        merge_is_never_offered_for_binary_or_unsupported_names();
    }

    fn merge_is_never_offered_for_binary_or_unsupported_names() {
        assert!(merge_supported_path(std::path::Path::new("notes.txt")));
        assert!(merge_supported_path(std::path::Path::new("legacy.QDOC")));
        assert!(merge_supported_path(std::path::Path::new("draft.TILO-DOC")));
        assert!(!merge_supported_path(std::path::Path::new("photo.png")));
        assert!(!merge_supported_path(std::path::Path::new("unknown")));
    }

    /// Each phase installs (or clears) its own thread-local opener and clears it
    /// again before the next phase.
    #[test]
    fn activation_hands_files_to_the_host_or_the_desktop_fallback() {
        activating_a_file_hands_the_path_to_the_host();
        a_refused_open_is_shown_to_the_user();
        without_a_host_every_type_falls_back_to_the_desktop_handler();
    }

    /// Every activated file goes to the host, whatever its type. The
    /// explorer must not sort files into "documents" and "everything
    /// else" — which app owns a `.pdf` or a `.png` is the desktop's
    /// answer, from what is installed, and a table here would be wrong
    /// the moment anybody installs anything.
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
            runner.opened.is_empty(),
            "and does not go behind the host's back to xdg-open"
        );
        assert_eq!(w.status, None, "a successful open reports nothing");
        clear_document_opener();
    }

    /// A host that cannot open the file says why, and the explorer shows
    /// it. Activation is never silent.
    fn a_refused_open_is_shown_to_the_user() {
        set_document_opener(|_| Err("no installed app opens .zzz files (thing.zzz)".into()));
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("thing.zzz")];
        let mut runner = RecRunner::default();
        assert!(w.open_entry(0, &mut runner).unwrap());
        let told = w.notice_line().expect("the refusal is said in a notice");
        assert!(told.contains("thing.zzz"), "must name the file: {told}");
        assert!(told.contains("no installed app"), "and say why: {told}");
        assert!(runner.opened.is_empty(), "a refusal is not a fallback");
        clear_document_opener();
    }

    /// With no host installed — the explorer run outside a Tilo session —
    /// activation still opens the file, via the desktop's own handler,
    /// rather than doing nothing.
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
            assert_eq!(runner.opened, vec![PathBuf::from("/docs").join(f)], "{f}");
            assert_eq!(
                w.status, None,
                "{f}: an open the desktop took reports nothing"
            );
        }

        // A desktop that refuses (the shell has no app for it, or the file
        // was never granted) is shown, not swallowed.
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("thing.zzz")];
        let mut runner = RecRunner {
            refusal: Some("No installed app opens .zzz files".into()),
            ..RecRunner::default()
        };
        assert!(w.open_entry(0, &mut runner).unwrap());
        assert_eq!(
            w.notice_line().as_deref(),
            Some(
                format!(
                    "{}. No installed app opens .zzz files",
                    t_args("files-open-failed", &[arg("name", "thing.zzz")])
                )
                .as_str()
            )
        );
    }

    pub(super) fn context_menu_opens_with_selection_aware_items() {
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
        for expected in ["Open", "Rename", "Cut", "Copy", "Move to Trash"] {
            assert!(
                labels.contains(&expected.to_string()),
                "menu missing {expected}: {labels:?}"
            );
        }
        // The menu of a file is about the file: what belongs to the folder
        // (new items, sorting, hidden files, a terminal) is the folder's menu.
        for folder_item in [
            "New folder",
            "Open terminal here",
            "Show hidden files",
            "Sort by name",
        ] {
            assert!(
                !labels.contains(&folder_item.to_string()),
                "a file's menu carries the folder item {folder_item}: {labels:?}"
            );
        }
        // No clipboard yet → no Paste.
        assert!(!labels.contains(&"Paste".to_string()));
    }

    /// No menu fences a single item off with a hairline, and none starts or
    /// ends on one: a separator stands only between two groups of at least
    /// two items.
    pub(super) fn menus_never_separate_a_lone_item() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt"), file_entry("b.txt")];
        for selected in [false, true] {
            w.selection.clear();
            if selected {
                w.selection.select_only(0);
            }
            w.open_context_menu(Point::new(10.0, 10.0));
            let separators: Vec<bool> = w
                .context_menu
                .items()
                .iter()
                .map(|item| item.label().is_none())
                .collect();
            assert_ne!(separators.first(), Some(&true));
            assert_ne!(separators.last(), Some(&true));
            let mut run = 0;
            let mut groups = Vec::new();
            for separator in separators {
                if separator {
                    groups.push(std::mem::take(&mut run));
                } else {
                    run += 1;
                }
            }
            groups.push(run);
            assert!(
                groups.len() == 1 || groups.iter().all(|size| *size >= 2),
                "selected={selected}: group sizes {groups:?}"
            );
        }
    }

    /// The folder's own menu, from empty space: new items, a terminal, and
    /// how the folder is shown (sorting with a check on the active key).
    pub(super) fn the_folder_menu_holds_the_folder_actions() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt")];
        w.open_context_menu(Point::new(10.0, 10.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(std::string::ToString::to_string))
            .collect();
        for expected in [
            "New folder",
            "Open terminal here",
            "Sort by name",
            "Sort by size",
            "Sort by type",
            "Sort by date",
            "Show hidden files",
        ] {
            assert!(
                labels.contains(&expected.to_string()),
                "{expected}: {labels:?}"
            );
        }
        assert!(!labels.contains(&"Rename".to_string()));
    }

    pub(super) fn empty_selection_menu_omits_item_actions() {
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
        assert!(labels.contains(&"New folder".to_string()));
        assert!(labels.contains(&"Open terminal here".to_string()));
    }

    pub(super) fn copy_then_paste_menu_includes_paste() {
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

    /// A host route that records what Files asked of it.
    struct RecordingRoute {
        apps: Vec<crate::open_with::OpenWithApp>,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl crate::open_with::OpenWithRoute for RecordingRoute {
        fn candidates(&self, _path: &Path) -> crate::open_with::OpenWithCandidates {
            self.calls.borrow_mut().push("candidates".into());
            crate::open_with::OpenWithCandidates {
                type_label: ".pdf files".into(),
                apps: self.apps.clone(),
            }
        }
        fn open_with(&self, path: &Path, app_id: &str) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("open {} {app_id}", path.display()));
            Ok(())
        }
        fn set_default(&self, path: &Path, app_id: &str) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("default {} {app_id}", path.display()));
            Ok(())
        }
    }

    impl RecordingRoute {
        fn actions(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .filter(|call| *call != "candidates")
                .cloned()
                .collect()
        }
    }

    struct ClearRoute;
    impl Drop for ClearRoute {
        fn drop(&mut self) {
            crate::open_with::clear_open_with_route();
            clear_document_opener();
        }
    }

    fn pdf_route() -> (Rc<RecordingRoute>, ClearRoute) {
        let route = Rc::new(RecordingRoute {
            apps: vec![
                crate::open_with::OpenWithApp {
                    id: "org.gnome.Evince.desktop".into(),
                    name: "Evince".into(),
                    is_default: true,
                },
                crate::open_with::OpenWithApp {
                    id: "so.tilo.pdf-viewer".into(),
                    name: "PDF Viewer".into(),
                    is_default: false,
                },
            ],
            calls: std::cell::RefCell::default(),
        });
        crate::open_with::set_open_with_route(route.clone());
        (route, ClearRoute)
    }

    fn menu_labels(menu: &ContextMenu) -> Vec<String> {
        menu.items()
            .iter()
            .filter_map(|item| item.label().map(str::to_string))
            .collect()
    }

    fn pdf_widget() -> FileExplorerWidget {
        let mut w = FileExplorerWidget::new();
        w.current_path = PathBuf::from("/docs");
        w.entries = vec![file_entry("slides.pdf"), file_entry("notes.txt")];
        w.selection.select_only(0);
        w
    }

    #[test]
    fn open_with_lists_the_default_first_and_opens_exactly_the_chosen_app() {
        let (route, _clear) = pdf_route();
        // Choosing an app never goes through activation's opener or the
        // standalone desktop handler.
        set_document_opener(|path| panic!("activation opener used for {}", path.display()));
        let mut w = pdf_widget();
        w.open_context_menu(Point::new(40.0, 40.0));
        assert!(menu_labels(&w.context_menu).contains(&"Open with".to_string()));

        w.open_open_with_submenu(Point::new(200.0, 40.0));
        assert_eq!(
            menu_labels(&w.submenu),
            vec!["Evince (default)", "PDF Viewer", "Other application…"]
        );
        let viewer = w.submenu_commands[1].clone().expect("an app row");
        assert_eq!(
            viewer,
            FileCommand::OpenWith {
                path: PathBuf::from("/docs/slides.pdf"),
                app_id: "so.tilo.pdf-viewer".into(),
                always: false,
            }
        );
        w.apply_command(viewer);
        assert_eq!(
            route.actions(),
            vec!["open /docs/slides.pdf so.tilo.pdf-viewer"]
        );
        assert_eq!(w.status, None, "a successful open reports nothing");
    }

    #[test]
    fn always_use_makes_the_app_the_default_before_opening() {
        let (route, _clear) = pdf_route();
        let mut w = pdf_widget();
        w.apply_command(FileCommand::OpenWith {
            path: PathBuf::from("/docs/slides.pdf"),
            app_id: "so.tilo.pdf-viewer".into(),
            always: true,
        });
        assert_eq!(
            route.actions(),
            vec![
                "default /docs/slides.pdf so.tilo.pdf-viewer",
                "open /docs/slides.pdf so.tilo.pdf-viewer",
            ]
        );
        assert_eq!(w.status.as_deref(), Some("PDF Viewer now opens .pdf files"));
    }

    #[test]
    fn open_with_is_offered_only_for_one_regular_file() {
        let (_route, _clear) = pdf_route();
        let mut w = pdf_widget();
        w.entries.push(Entry {
            is_dir: true,
            ..file_entry("Folder")
        });
        w.selection.select_only(2);
        assert_eq!(w.open_with_subject(), None, "folders open in Files");
        w.selection.select_only(0);
        w.selection.toggle(1);
        assert_eq!(w.open_with_subject(), None, "one file at a time");
    }

    #[test]
    fn without_a_route_open_with_says_so_and_launches_nothing() {
        crate::open_with::clear_open_with_route();
        let mut w = pdf_widget();
        w.apply_command(FileCommand::OpenWith {
            path: PathBuf::from("/docs/slides.pdf"),
            app_id: "so.tilo.pdf-viewer".into(),
            always: false,
        });
        assert!(
            w.status
                .as_deref()
                .is_some_and(|status| status.contains("slides.pdf")),
            "{:?}",
            w.status
        );
        w.open_open_with_submenu(Point::new(10.0, 10.0));
        assert_eq!(
            menu_labels(&w.submenu),
            vec!["No application opens this file"]
        );
    }

    #[test]
    fn open_with_is_a_typed_action_on_file_resources() {
        let (route, _clear) = pdf_route();
        let mut w = pdf_widget();
        let mut doc = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(&w, &doc);
        let entry_id = crate::search_provider::entry_resource_id(Path::new("/docs/slides.pdf"));
        let spec = ui::tilo_actions::door::resolve(
            &resources,
            &ui::tilo_actions::ActionAddress::new(&entry_id, "open-with"),
        )
        .expect("file resources publish open-with");
        let app = spec
            .params()
            .iter()
            .find(|param| param.name == "app")
            .expect("an app parameter");
        assert_eq!(
            app.choices
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["org.gnome.Evince.desktop", "so.tilo.pdf-viewer"]
        );
        let mut refused = serde_json::Map::new();
        refused.insert("app".into(), "rm.desktop".into());
        assert!(spec.build_json(&refused).is_err(), "only a candidate binds");

        let mut args = serde_json::Map::new();
        args.insert("app".into(), "so.tilo.pdf-viewer".into());
        args.insert("always".into(), true.into());
        let encoded = spec.build_json(&args).unwrap();
        assert!(matches!(
            crate::files_app::published::start(
                &mut w,
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
                &mut doc,
                &automerge::ROOT
            )
            .unwrap(),
            Some(ui::framework::Effected::Done(_))
        ));
        assert_eq!(
            route.actions(),
            vec![
                "default /docs/slides.pdf so.tilo.pdf-viewer",
                "open /docs/slides.pdf so.tilo.pdf-viewer",
            ]
        );
        let folder = crate::search_provider::entry_resource_id(Path::new("/docs/notes.txt"));
        assert!(
            ui::tilo_actions::door::resolve(
                &resources,
                &ui::tilo_actions::ActionAddress::new(&folder, "open-with:so.tilo.pdf-viewer"),
            )
            .is_none(),
            "per-app rows are only published for the chooser's file"
        );
    }

    #[test]
    fn other_application_searches_every_app_to_open_once_or_always() {
        let (route, _clear) = pdf_route();
        let mut w = pdf_widget();
        w.apply_command(FileCommand::OpenWithOther);
        assert!(w.app_chooser_open(), "the shared palette opens");
        assert!(Widget::traps_focus(&w), "and owns the keyboard");
        let rows: Vec<(String, Option<String>)> = w
            .app_chooser
            .visible_entries()
            .into_iter()
            .map(|row| (row.label.clone(), row.group.clone()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Evince".to_string(), Some("Open once".to_string())),
                ("PDF Viewer".to_string(), Some("Open once".to_string())),
                (
                    "Evince".to_string(),
                    Some("Always use for .pdf files".to_string())
                ),
                (
                    "PDF Viewer".to_string(),
                    Some("Always use for .pdf files".to_string())
                ),
            ]
        );
        // Picking a row resolves through the published resource, exactly as
        // the application host dispatches a palette selection.
        let mut doc = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(&w, &doc);
        let entry_id = crate::search_provider::entry_resource_id(Path::new("/docs/slides.pdf"));
        let spec = ui::tilo_actions::door::resolve(
            &resources,
            &ui::tilo_actions::ActionAddress::new(
                &entry_id,
                "always-open-with:org.gnome.Evince.desktop",
            ),
        )
        .expect("the chooser's rows are real actions");
        let encoded = spec.build_json(&serde_json::Map::new()).unwrap();
        crate::files_app::published::start(
            &mut w,
            spec.kind,
            &encoded.type_id,
            &encoded.payload,
            &mut doc,
            &automerge::ROOT,
        )
        .unwrap();
        assert_eq!(
            route.actions(),
            vec![
                "default /docs/slides.pdf org.gnome.Evince.desktop",
                "open /docs/slides.pdf org.gnome.Evince.desktop",
            ]
        );
        assert!(!w.app_chooser_open(), "a pick closes the chooser");
    }

    #[test]
    fn candidates_are_asked_once_per_type_per_listing() {
        let (route, _clear) = pdf_route();
        let w = pdf_widget();
        let doc = automerge::AutoCommit::new();
        let _ = crate::files_app::published::tree(&w, &doc);
        let _ = crate::files_app::published::tree(&w, &doc);
        let asked = route
            .calls
            .borrow()
            .iter()
            .filter(|call| *call == "candidates")
            .count();
        assert_eq!(asked, 2, "one ask for .pdf and one for .txt, then cached");
    }

    fn displayed_names(w: &FileExplorerWidget) -> Vec<String> {
        w.displayed_entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    #[test]
    fn recent_lists_what_you_opened_newest_first() {
        let config = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(folder.path().join(name), name).unwrap();
        }
        set_document_opener(|_| Ok(()));
        let _clear = ClearRoute;
        let mut w = FileExplorerWidget::with_persistence(Some(config.path()), None);
        w.navigate(folder.path().to_path_buf());
        assert_eq!(displayed_names(&w), vec!["a.txt", "b.txt", "c.txt"]);
        let mut runner = RecRunner::default();
        for name in ["b.txt", "a.txt", "c.txt", "a.txt"] {
            let index = displayed_names(&w)
                .iter()
                .position(|candidate| candidate == name)
                .unwrap();
            assert!(w.open_entry(index, &mut runner).unwrap());
        }
        assert!(runner.opened.is_empty(), "the host opened every file");

        w.apply_command(FileCommand::Navigate(crate::recent::location()));
        assert!(w.error.is_none(), "{:?}", w.error);
        assert_eq!(
            displayed_names(&w),
            vec!["a.txt", "c.txt", "b.txt"],
            "recency order, not the folder's name sort"
        );
        assert_eq!(
            crate::breadcrumbs::breadcrumb_components(&w.current_path, None, &[])
                .into_iter()
                .map(|(label, _)| label)
                .collect::<Vec<_>>(),
            vec!["Recent"]
        );
    }

    /// The Recent list is this device's setting: a reopened window lists it.
    #[test]
    fn recent_is_a_device_setting_that_survives_a_restart() {
        let folder = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(folder.path().join(name), name).unwrap();
        }
        let mut app = ui::prelude::TestApp::<crate::FileExplorerShell>::launched(&[folder
            .path()
            .to_str()
            .unwrap()])
        .unwrap();
        app.with_surface(|files: &mut FileExplorerWidget| {
            for name in ["b.txt", "a.txt"] {
                files.note_recent(&folder.path().join(name), crate::recent::RecentUse::Opened);
            }
        })
        .unwrap();
        let stored = app.settings::<crate::recent::RecentFiles>().get();
        assert_eq!(
            stored
                .items()
                .iter()
                .map(|item| item.path.clone())
                .collect::<Vec<_>>(),
            vec![folder.path().join("a.txt"), folder.path().join("b.txt")]
        );
        let mut app = app.reopen().unwrap();
        let names = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files
                    .recent_entries()
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn recent_forgets_deleted_files_and_on_request() {
        let config = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        for name in ["keep.txt", "gone.txt", "drop.txt"] {
            std::fs::write(folder.path().join(name), name).unwrap();
        }
        let mut w = FileExplorerWidget::with_persistence(Some(config.path()), None);
        for name in ["keep.txt", "gone.txt", "drop.txt"] {
            w.note_recent(&folder.path().join(name), crate::recent::RecentUse::Changed);
        }
        std::fs::remove_file(folder.path().join("gone.txt")).unwrap();
        w.navigate(crate::recent::location());
        assert_eq!(displayed_names(&w), vec!["drop.txt", "keep.txt"]);

        w.selection.select_only(0);
        w.open_context_menu(Point::new(20.0, 20.0));
        let labels = menu_labels(&w.context_menu);
        assert!(
            labels.contains(&"Open containing folder".to_string()),
            "{labels:?}"
        );
        assert!(labels.contains(&"Remove from Recent".to_string()));
        assert!(!labels.contains(&"New folder".to_string()), "{labels:?}");
        assert!(!labels.contains(&"Paste".to_string()));

        w.apply_command(FileCommand::ForgetRecent(folder.path().join("drop.txt")));
        assert_eq!(displayed_names(&w), vec!["keep.txt"]);
        assert!(folder.path().join("drop.txt").exists(), "the file stays");

        w.apply_command(FileCommand::NewFolder);
        assert_eq!(
            w.status.as_deref(),
            Some("Recent is not a folder; open one to add files")
        );

        w.selection.select_only(0);
        w.apply_command(FileCommand::RevealInFolder(folder.path().join("keep.txt")));
        assert_eq!(w.current_path, folder.path());
        let selected = w.selection.primary().unwrap();
        assert_eq!(w.displayed_entries()[selected].name, "keep.txt");
    }

    #[test]
    fn a_rename_keeps_the_file_in_recent() {
        let config = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("draft.txt"), "x").unwrap();
        let mut w = FileExplorerWidget::with_persistence(Some(config.path()), None);
        w.navigate(folder.path().to_path_buf());
        w.note_recent(
            &folder.path().join("draft.txt"),
            crate::recent::RecentUse::Opened,
        );
        w.selection.select_only(0);
        w.begin_rename();
        w.set_rename_buffer("final.txt".into());
        assert!(w.commit_rename());
        assert_eq!(
            w.recent_entries()
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>(),
            vec!["final.txt"]
        );
    }
    mod retained_open_with_tests {
        use super::*;
        use ui::collaboration::{CollabEvent, CollabRequest, open_selected};

        fn discovered_files() -> FileExplorerWidget {
            crate::open_with::clear_open_with_route();
            let mut files = FileExplorerWidget::new();
            files.current_path = PathBuf::from("/run/tilo/retained-home/Documents");
            let mut entry = file_entry("report.md");
            entry.path = files.current_path.join("report.md");
            files.entries = vec![entry];
            files.dispatcher = Some(ui::framework::testing::dispatcher::<
                crate::resource::FileExplorer,
            >("so.tilo.file-explorer"));
            assert!(
                files
                    .open_with_candidates(&files.entries[0].path)
                    .apps
                    .is_empty()
            );
            let CollabRequest::SelectedDiscovery {
                request_id,
                command,
                on_behalf_of,
            } = files.selected_open.discovery.take_request().unwrap()
            else {
                panic!("the actual Files discovery request is required");
            };
            assert!(on_behalf_of.is_none());
            let reply = open_selected::discovery::Reply {
                schema: 1,
                request_id: command.request_id,
                components: command.components,
                type_label: "Markdown document".into(),
                source_operation: Some(uuid::Uuid::new_v4().to_string()),
                targets: vec![open_selected::discovery::Target {
                    app_id: "so.tilo.word-processor".into(),
                    package_sha256: "a".repeat(64),
                    name: "Writer".into(),
                    is_default: true,
                    access: open_selected::v2::SelectedAccess::EditInParent,
                }],
            };
            assert!(
                files
                    .selected_open
                    .discovery
                    .event(&CollabEvent::SelectedDiscoveryResult {
                        request_id,
                        result: Ok(reply)
                    })
            );
            files
        }

        #[test]
        fn retained_open_with_publishes_explicit_edit_and_defaults_to_read() {
            let mut files = discovered_files();
            let doc = automerge::AutoCommit::new();
            let resource = FileExplorerWidget::entry_resource_id(&files.entries[0]);
            let resources = crate::files_app::published::tree(&files, &doc);
            let spec = ui::tilo_actions::door::resolve(
                &resources,
                &ui::tilo_actions::ActionAddress::new(&resource, "open-with"),
            )
            .unwrap();
            let access = spec
                .params()
                .iter()
                .find(|param| param.name == "access")
                .unwrap();
            assert_eq!(
                access
                    .choices
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                vec!["read-only", "edit-in-parent"]
            );
            assert_eq!(access.schema.as_ref().unwrap()["default"], "read-only");
            let mut args =
                serde_json::Map::from_iter([("app".into(), "so.tilo.word-processor".into())]);
            let encoded = spec.build_json(&args).unwrap();
            assert!(
                matches!(crate::effects::FilesEffect::from_payload(&encoded.payload).unwrap(), crate::effects::FilesEffect::OpenWith { access: Some(access), .. } if access == "read-only")
            );
            args.insert("access".into(), "edit-in-parent".into());
            args.insert("always".into(), false.into());
            let encoded = spec.build_json(&args).unwrap();
            let effect = crate::effects::FilesEffect::from_payload(&encoded.payload).unwrap();
            assert!(
                matches!(&effect, crate::effects::FilesEffect::OpenWith { access: Some(access), .. } if access == "edit-in-parent")
            );
            let invocation = ui::tilo_actions::InvocationId(uuid::Uuid::new_v4());
            assert!(matches!(
                files.run_effect_for(effect, Some(invocation)).unwrap(),
                Ran::Pending
            ));
            let CollabRequest::SelectedDiscovery {
                command,
                on_behalf_of,
                ..
            } = files.selected_open.discovery.take_request().unwrap()
            else {
                panic!("Edit must renew the original action's discovery scope");
            };
            assert_eq!(on_behalf_of, Some(invocation));
            let scope = command.invocation.unwrap();
            assert_eq!(scope.resource, resource);
            assert_eq!(scope.app_id, "so.tilo.word-processor");
            assert_eq!(
                scope.access,
                open_selected::v2::SelectedAccess::EditInParent
            );
            assert!(
                files.selected_open.take_request().is_none(),
                "presentation never grants file descriptors or launches an editor"
            );
        }

        #[test]
        fn retained_edit_without_original_invocation_never_falls_back_to_path_launch() {
            let mut files = discovered_files();
            let resource = FileExplorerWidget::entry_resource_id(&files.entries[0]);
            let error = files
                .run_effect_for(
                    crate::effects::FilesEffect::OpenWith {
                        entry: resource,
                        app: "so.tilo.word-processor".into(),
                        always: Some(false),
                        access: Some("edit-in-parent".into()),
                    },
                    None,
                )
                .err()
                .expect("an original semantic action is mandatory");
            assert_eq!(error, t("files-open-with-action-unavailable"));
            assert!(files.selected_open.take_request().is_none());
            assert!(files.selected_open.discovery.take_request().is_none());
            assert!(!files.selected_open.is_pending());
        }
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

    /// Wake scheduling, record-preserving metadata refresh and record-id search
    /// reveal — each phase with its own fixture and scoped environment.
    #[test]
    fn scheduled_refresh_and_record_search_preserve_the_view() {
        opaque_hold_deadline_reaches_files_root_before_metadata_refresh();
        #[cfg(unix)]
        scheduled_metadata_refresh_preserves_record_selection_and_scroll();
        #[cfg(unix)]
        collaborative_search_activation_resolves_the_latest_path_by_record_id();
    }

    /// A metadata poll is real work, but an unchanged directory
    /// must not look like a frame request. The native runner used to turn
    /// any due `next_wake_deadline` into a Wayland commit (~2 paints/s).
    #[cfg(unix)]
    #[test]
    fn idle_directory_refresh_does_not_request_frames() {
        use std::collections::HashMap;
        use taffy::prelude::*;
        use ui::theme::Theme;
        use vello::kurbo::Point;

        // As on the desktop smoke's runner: no identity projection directory.
        let mut env = tilo_test_env::ScopedEnv::new();
        let _identities = absent_identity_projection(&mut env);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"ok").unwrap();
        let mut widget = FileExplorerWidget::new();
        widget.load(dir.path().to_path_buf());
        let mut state = ui::State::new();
        let mut tr = ui::text_render::TextRenderer::new();
        let theme = Theme::dark();
        let lay_out = |widget: &mut FileExplorerWidget,
                       state: &mut ui::State,
                       tr: &mut ui::text_render::TextRenderer| {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            widget.sync(&state.doc, &state.root);
            let node = widget.build_layout(&mut taffy, &theme, &mut nm);
            let measure_map = nm.clone();
            taffy
                .compute_layout_with_measure(
                    node,
                    Size {
                        width: AvailableSpace::Definite(1100.0),
                        height: AvailableSpace::Definite(800.0),
                    },
                    |k, a, id, _, _| {
                        if let Some(&p) = measure_map.get(&id) {
                            unsafe { (*p).measure(k, a, tr) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            widget.apply_layout(&taffy, node, Point::ORIGIN);
        };
        lay_out(&mut widget, &mut state, &mut tr);
        if widget.take_structure_dirty() {
            lay_out(&mut widget, &mut state, &mut tr);
        }
        ui::widget::Widget::mark_drawn(&mut widget);
        let _ = widget.take_structure_dirty();

        widget.next_metadata_refresh = Some(ui::time::now());
        let _ = widget.refresh_metadata_if_due(ui::time::now());
        widget.next_metadata_refresh = Some(ui::time::now());
        assert!(
            !widget.refresh_metadata_if_due(ui::time::now()),
            "an idle Files poll must not keep requesting frames"
        );

        // Production AboutToWait: update + sync on the due deadline. Files
        // stays `needs_redraw` after `mark_drawn`; the runner must ignore
        // that bit. A model/structure/animation change is a real present.
        ui::widget::Widget::mark_drawn(&mut widget);
        let _ = widget.take_structure_dirty();
        assert_idle_turn_is_clean(&mut widget, &state, "idle scheduled sync");

        // Nearby Send used to ask every 2s; now only on an announced change.
        assert!(
            !widget.take_collab_requests().iter().any(|request| matches!(
                request,
                ui::collaboration::CollabRequest::NearbyTransferCommand { .. }
            )),
            "an idle Files window must not ask Nearby Send anything"
        );
        ui::widget::Collaborative::handle_collab_event(
            &mut widget,
            &ui::collaboration::CollabEvent::NearbyTransferChanged,
        );
        let request_id = widget
            .take_collab_requests()
            .iter()
            .find_map(|request| match request {
                ui::collaboration::CollabRequest::NearbyTransferCommand {
                    request_id,
                    command: ui::collaboration::NearbyTransferCommand::Snapshot,
                    ..
                } => Some(*request_id),
                _ => None,
            })
            .expect("an announced change asks for a snapshot");
        ui::widget::Collaborative::handle_collab_event(
            &mut widget,
            &ui::collaboration::CollabEvent::NearbyTransferCommandResult {
                request_id,
                result: Ok(ui::collaboration::NearbyTransferReply::Snapshot {
                    snapshot: ui::collaboration::NearbyTransferSnapshot::default(),
                }),
            },
        );
        assert_idle_turn_is_clean(&mut widget, &state, "idle Nearby snapshot");
        assert_watched_idle_window_needs_no_wake(&mut widget);
    }

    /// The host's turn on a due deadline (update, then sync) found nothing to
    /// present: no model, structure or animation change.
    #[cfg(unix)]
    fn assert_idle_turn_is_clean(widget: &mut FileExplorerWidget, state: &ui::State, what: &str) {
        let updated = ui::widget::Widget::update(widget, &state.doc, &state.root);
        ui::widget::Widget::sync(widget, &state.doc, &state.root);
        let structure = ui::widget::Widget::take_structure_dirty(widget);
        let needs_animation = ui::widget::Widget::needs_animation_frame(widget);
        assert!(
            !updated && !structure && !needs_animation,
            "{what} still dirty: updated={updated} structure={structure} anim={needs_animation}"
        );
    }

    /// Once the forced polls are done, a window whose directory and identity
    /// projection are watched asks the host for no wake at all.
    #[cfg(unix)]
    fn assert_watched_idle_window_needs_no_wake(widget: &mut FileExplorerWidget) {
        widget.next_metadata_refresh = None;
        let _ = widget.refresh_metadata_if_due(ui::time::now());
        assert_eq!(
            widget.next_wake_deadline(),
            widget.next_metadata_refresh,
            "nothing but a metadata poll may schedule an idle window"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            widget.next_wake_deadline(),
            None,
            "a fully watched idle Files window never wakes the host"
        );
    }

    /// Point the identity projection and the workspace roots at fixtures,
    /// so the only refresh sources are the ones the test controls.
    #[cfg(unix)]
    fn quiet_refresh_sources(env: &mut tilo_test_env::ScopedEnv) -> tempfile::TempDir {
        let identities = tempfile::tempdir().unwrap();
        env.set(
            crate::identity_directory::TRUSTED_IDENTITIES_ENV,
            identities.path().join("trusted-identities.json"),
        );
        env.set("TILO_WORKSPACE_ROOTS", "");
        identities
    }

    /// Point the identity projection at a directory that does not exist, as
    /// on a host or sandbox without one, and the workspace roots at nothing.
    #[cfg(unix)]
    fn absent_identity_projection(env: &mut tilo_test_env::ScopedEnv) -> tempfile::TempDir {
        let identities = tempfile::tempdir().unwrap();
        env.set(
            crate::identity_directory::TRUSTED_IDENTITIES_ENV,
            identities
                .path()
                .join("identity-projection/trusted-identities.json"),
        );
        env.set("TILO_WORKSPACE_ROOTS", "");
        identities
    }

    /// A missing identity projection is not polled: its creation is awaited
    /// on the nearest directory that exists. A projection that appears later
    /// is still read, without any poll falling due.
    #[cfg(unix)]
    #[test]
    fn a_missing_identity_projection_is_awaited_not_polled() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let identities = absent_identity_projection(&mut env);
        let dir = tempfile::tempdir().unwrap();
        let mut widget = FileExplorerWidget::new();
        widget.load(dir.path().to_path_buf());
        let now = ui::time::now();
        let _ = widget.refresh_metadata_if_due(now);
        assert!(widget.trusted_humans.humans().is_empty());
        assert!(!widget.identity_watch.watches_target());
        assert_ne!(
            widget.identity_watch.coverage(),
            crate::directory_watch::Coverage::None,
            "the projection's creation is watched for"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            widget.next_metadata_refresh, None,
            "a missing projection on a local filesystem is never polled"
        );

        let projection = identities.path().join("identity-projection");
        std::fs::create_dir(&projection).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32])
            .verifying_key()
            .to_bytes();
        std::fs::write(
            projection.join("trusted-identities.json"),
            serde_json::to_vec(&tilo_user_identity::TrustedIdentityProjectionV1 {
                schema_version: tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_V1_VERSION,
                humans: vec![tilo_user_identity::GovernedHumanIdentityV1 {
                    root_public_key: root,
                    display_name: "Ada".into(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        // The same `now` every turn: no poll can fall due, so only the
        // watch can bring the projection in.
        tilo_test_env::wait_until("the new identity projection is read", || {
            let _ = widget.refresh_metadata_if_due(now);
            !widget.trusted_humans.humans().is_empty()
        });
        assert_eq!(widget.trusted_humans.humans()[0].display_name, "Ada");
        assert!(
            widget.identity_watch.watches_target(),
            "the watch moved onto the projection's directory"
        );
    }

    /// An idle window over an unchanged local directory has no metadata
    /// deadline at all: its watch wakes it on change, so nothing is re-read
    /// on a timer. Where the watch cannot vouch for every change, the poll
    /// stays, never faster than the fallback interval.
    #[cfg(unix)]
    #[test]
    fn idle_files_has_no_periodic_deadline_with_a_subscription() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let _identities = quiet_refresh_sources(&mut env);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"ok").unwrap();
        let mut widget = FileExplorerWidget::new();
        widget.load(dir.path().to_path_buf());
        let now = ui::time::now();
        let _ = widget.refresh_metadata_if_due(now);
        assert!(!widget.directory_changed, "nothing changed");
        assert_ne!(
            widget.directory_watch.coverage(),
            crate::directory_watch::Coverage::None,
            "the open directory is watched"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            widget.next_metadata_refresh, None,
            "a fully watched directory is never polled"
        );
        if let Some(due) = widget.next_metadata_refresh {
            assert!(
                due >= now + metadata_refresh::LIVE_INTERVAL,
                "only a partially watched directory keeps its poll"
            );
        }
    }

    /// A file another program creates in the open directory shows, read
    /// because the watch reported it, before any poll is due.
    #[cfg(unix)]
    #[test]
    fn a_file_created_in_the_open_directory_appears_without_a_poll() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let _identities = quiet_refresh_sources(&mut env);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"ok").unwrap();
        let mut widget = FileExplorerWidget::new();
        widget.load(dir.path().to_path_buf());
        let _ = widget.refresh_metadata_if_due(ui::time::now());
        let created = ui::time::now();
        std::fs::write(dir.path().join("b.txt"), b"new").unwrap();
        let shown =
            |widget: &FileExplorerWidget| widget.entries.iter().any(|entry| entry.name == "b.txt");
        while !shown(&widget) && created.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            // The earliest moment a reported change may be read, which is
            // before any poll is due: only the watch can make this read.
            let readable = widget.last_directory_read
                + metadata_refresh::change_read_gap(widget.entries.len());
            if let Some(due) = widget.next_metadata_refresh {
                assert!(readable < due, "the change is read before the poll");
            }
            let _ = widget.refresh_metadata_if_due(readable);
        }
        // How soon is `change_read_gap`'s (under a second); wall time here
        // would only measure the test machine's load.
        assert!(shown(&widget), "the new file is listed");
        assert!(!widget.directory_changed, "the reported change was read");
    }

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
        widget.next_metadata_refresh = Some(deadline + std::time::Duration::from_secs(60));
        assert!(
            widget
                .next_wake_deadline()
                .is_some_and(|wake| wake <= deadline),
            "the root must not schedule later than its scroll fade"
        );
        let refresh = deadline
            .checked_sub(std::time::Duration::from_millis(1))
            .expect("deadline lies in the future");
        widget.next_metadata_refresh = Some(refresh);
        assert!(
            widget
                .next_wake_deadline()
                .is_some_and(|wake| wake <= refresh),
            "the root must not schedule later than its metadata refresh"
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
            observed: None,
        }
    }

    /// Sort, location bar, shortcut reference, zoom and hidden-file toggles, each
    /// phase on a fresh widget.
    #[test]
    fn view_commands_are_reachable_from_keyboard_and_command_paths() {
        set_sort_command_selects_the_key_including_type();
        command_l_opens_the_editable_location_bar_without_a_pointer();
        shortcut_reference_is_a_keyboard_and_command_reachable_view();
        zoom_commands_step_the_item_size_and_clamp();
        toggle_hidden_reveals_and_hides_dotfiles();
    }

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

    fn command_l_opens_the_editable_location_bar_without_a_pointer() {
        use winit::keyboard::{Key, ModifiersState};

        let mut widget = FileExplorerWidget::new();
        let event = ui::synthetic_input::key_event(
            Key::Character(winit::keyboard::SmolStr::new("l")),
            None,
            winit::event::ElementState::Pressed,
        );

        assert!(widget.handle_app_key(
            &event,
            winit::event::Modifiers::from(ModifiersState::CONTROL),
        ));
        assert!(
            widget.toolbar.is_path_editing(),
            "the documented location shortcut must expose the real editable path control"
        );
    }

    fn shortcut_reference_is_a_keyboard_and_command_reachable_view() {
        use winit::keyboard::{Key, NamedKey};
        let mut widget = FileExplorerWidget::new();

        let event = ui::synthetic_input::key_event(
            Key::Named(NamedKey::F1),
            None,
            winit::event::ElementState::Pressed,
        );
        assert!(widget.handle_app_key(&event, winit::event::Modifiers::default()));
        let panel = widget
            .collab_panel
            .as_ref()
            .expect("shortcut reference opens");
        assert_eq!(panel.title, "Keyboard shortcuts");
        // Real chords with the platform's key names, never narration
        // about modifiers.
        assert!(
            panel.lines.iter().any(|line| line == "Rename: F2"),
            "{:?}",
            panel.lines
        );
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line == "New folder: Ctrl+Shift+N"),
            "{:?}",
            panel.lines
        );
        assert!(
            panel.lines.iter().all(|line| !line.contains("Primary")),
            "{:?}",
            panel.lines
        );
    }

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
            labels.contains(&"Delete permanently".to_string()),
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

    /// The folder resource's action catalogue, contracts, paging and ids, each
    /// phase on a fresh widget and document.
    #[test]
    fn folder_resource_exports_canonical_actions_handlers_and_ids() {
        resource_catalogue_exposes_view_and_favourite_commands_through_the_action_door();
        every_exported_local_file_handler_is_declared_for_the_broker();
        exported_background_operations_have_valid_job_contracts();
        resource_catalogue_is_bounded_but_an_exact_visible_entry_resolves_lazily();
        template_resource_ids_are_stable_bounded_and_do_not_disclose_paths();
        a_mounted_remote_account_opens_even_offline_but_not_when_disabled();
    }

    fn resource_catalogue_exposes_view_and_favourite_commands_through_the_action_door() {
        let state_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let folder = home_dir.path().join("agent-folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("b.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::with_persistence(
            Some(state_dir.path()),
            Some(home_dir.path().to_path_buf()),
        );
        w.navigate(folder);
        let initial_zoom = w.zoom_level();
        let mut doc = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(&w, &doc);
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
        ] {
            assert!(
                folder_resource.actions.iter().any(|spec| spec.id == action),
                "folder resource must export {action}"
            );
        }

        for command in ["grid", "sort-size", "zoom-in", "keyboard-shortcuts"] {
            assert!(
                matches!(
                    crate::files_app::published::invoke(
                        &mut w,
                        &mut doc,
                        folder_resource.id.as_str(),
                        command,
                        &serde_json::json!({}),
                    )
                    .unwrap(),
                    Some(ui::framework::Effected::Done(_))
                ),
                "{command} is routed through the canonical door"
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
    }

    fn large_text_view_menu_clamps_inside_the_live_window() {
        let mut w = FileExplorerWidget::new();
        w.rect = vello::kurbo::Rect::new(0.0, 0.0, 1440.0, 900.0);
        let mut menu = w.new_overlay_menu();
        menu.push(MenuItem::action(t("files-menu-remove-favourite"), || {}));
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
            "the scaled menu keeps enough width for its label: {menu:?}"
        );
    }

    #[test]
    fn one_time_cloud_import_is_an_independent_copy_through_the_action_door() {
        for kind in [
            crate::remote_accounts::RemoteAccountKind::Dropbox,
            crate::remote_accounts::RemoteAccountKind::GoogleDrive,
        ] {
            one_time_cloud_import_for_kind(kind);
        }
    }

    fn one_time_cloud_import_for_kind(kind: crate::remote_accounts::RemoteAccountKind) {
        let home = tempfile::tempdir().unwrap();
        let mount = home.path().join("Remote/Cloud");
        let destination = home.path().join("Downloads");
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::create_dir(mount.join("Folder")).unwrap();
        std::fs::write(mount.join("Folder/document.txt"), b"original bytes").unwrap();
        // Provider-native documents are already projected as link files.
        std::fs::write(
            mount.join("Folder/document.gdoc"),
            b"{\"url\":\"https://docs.google.com/document/d/test\"}",
        )
        .unwrap();
        let mut account = remote_account(
            crate::remote_accounts::RemoteAccountHealth::Healthy,
            Some(mount.clone()),
        );
        account.kind = kind;
        let mut app =
            ui::prelude::TestApp::<crate::FileExplorerShell>::launched(&[mount.to_str().unwrap()])
                .unwrap();
        let row = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files.home_path = Some(home.path().to_path_buf());
                files.user_dirs = favorites::UserDirs {
                    downloads: Some(destination.clone()),
                    ..Default::default()
                };
                files.startup_accounts = Some(vec![account]);
                files.navigate(mount.clone());
                files
                    .published_rows()
                    .into_iter()
                    .find(|row| row.label == "Folder")
                    .unwrap()
            })
            .unwrap();
        assert!(row.actions.iter().any(|action| action.id == "import-once"));
        app.call_on(
            row.id.as_str(),
            "import-once",
            serde_json::json!({"folder": destination}),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !destination.join("Folder/document.txt").exists()
            || app.state().ops.pending().next().is_some()
        {
            assert!(std::time::Instant::now() < deadline);
            app.settle();
            std::thread::yield_now();
        }
        assert_eq!(
            std::fs::read(destination.join("Folder/document.txt")).unwrap(),
            b"original bytes"
        );
        assert_eq!(
            std::fs::read(destination.join("Folder/document.gdoc")).unwrap(),
            std::fs::read(mount.join("Folder/document.gdoc")).unwrap()
        );
        std::fs::write(mount.join("Folder/document.txt"), b"later remote edit").unwrap();
        assert_eq!(
            std::fs::read(destination.join("Folder/document.txt")).unwrap(),
            b"original bytes"
        );
        std::fs::write(
            destination.join("Folder/document.txt"),
            b"local import edit",
        )
        .unwrap();
        assert_eq!(
            std::fs::read(mount.join("Folder/document.txt")).unwrap(),
            b"later remote edit"
        );
        app.with_surface(|files: &mut FileExplorerWidget| {
            let index = files
                .displayed_entries()
                .iter()
                .position(|entry| entry.name == "Folder")
                .unwrap();
            files.select(Some(index));
            assert!(
                files
                    .context_menu_spec(&[])
                    .iter()
                    .all(|(label, _)| !label.contains("Downloads")),
                "cloud import never chooses a destination on the person's behalf"
            );
        })
        .unwrap();
        // A symlink cannot turn the local destination into a cloud folder.
        let link = home.path().join("cloud-link");
        std::os::unix::fs::symlink(&mount, &link).unwrap();
        assert!(
            app.call_on(
                row.id.as_str(),
                "import-once",
                serde_json::json!({"folder": link})
            )
            .is_err()
        );
    }

    #[test]
    fn cloud_import_retains_local_destination_after_alias_changes() {
        let home = tempfile::tempdir().unwrap();
        let mount = home.path().join("Cloud");
        let destination = home.path().join("Downloads");
        let alias = home.path().join("destination-alias");
        std::fs::create_dir(&mount).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(mount.join("document.txt"), b"remote bytes").unwrap();
        std::os::unix::fs::symlink(&destination, &alias).unwrap();
        let mut account = remote_account(
            crate::remote_accounts::RemoteAccountHealth::Healthy,
            Some(mount.clone()),
        );
        account.kind = crate::remote_accounts::RemoteAccountKind::Dropbox;
        let mut app =
            ui::prelude::TestApp::<crate::FileExplorerShell>::launched(&[mount.to_str().unwrap()])
                .unwrap();
        // Hold the surface's submission until the admitted alias is redirected.
        // Execution below still goes through the receiving host's checked door.
        let row = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files.home_path = Some(home.path().to_path_buf());
                files.startup_accounts = Some(vec![account]);
                files.navigate(mount.clone());
                files.dispatcher = Some(ui::framework::testing::dispatcher::<
                    crate::resource::FileExplorer,
                >("files"));
                files
                    .published_rows()
                    .into_iter()
                    .find(|row| row.label == "document.txt")
                    .unwrap()
            })
            .unwrap();
        app.call_on(
            row.id.as_str(),
            "import-once",
            serde_json::json!({"folder": alias}),
        )
        .unwrap();
        assert!(!destination.join("document.txt").exists());
        let op = app
            .with_surface(|files: &mut FileExplorerWidget| {
                assert_eq!(files.awaiting_ops.len(), 1);
                files.awaiting_ops[0].0.clone()
            })
            .unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&mount, &alias).unwrap();
        app.invoke(crate::resource::FileExplorer::queueing("test-device", op).unwrap())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.state().ops.pending().next().is_some() {
            assert!(std::time::Instant::now() < deadline);
            app.settle();
            std::thread::yield_now();
        }
        let state = app.state();
        let outcome = state
            .ops
            .entries()
            .next()
            .unwrap()
            .1
            .settled
            .as_ref()
            .unwrap();
        assert!(
            matches!(outcome, ui::tilo_resource::Settled::Done(_)),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read(destination.join("document.txt")).unwrap(),
            b"remote bytes"
        );
        assert!(!mount.join("document 2.txt").exists());
        std::fs::write(mount.join("document.txt"), b"remote edit").unwrap();
        assert_eq!(
            std::fs::read(destination.join("document.txt")).unwrap(),
            b"remote bytes"
        );
        std::fs::write(destination.join("document.txt"), b"local edit").unwrap();
        assert_eq!(
            std::fs::read(mount.join("document.txt")).unwrap(),
            b"remote edit"
        );

        // The action schema requires an absolute folder, even when a
        // submitted relative folder exists and canonicalizes successfully.
        assert!(
            app.call_on(
                row.id.as_str(),
                "import-once",
                serde_json::json!({"folder": "."})
            )
            .is_err()
        );
        assert_eq!(app.state().ops.entries().count(), 1);
    }

    fn remote_account(
        health: crate::remote_accounts::RemoteAccountHealth,
        mount: Option<PathBuf>,
    ) -> crate::remote_accounts::RemoteAccount {
        crate::remote_accounts::RemoteAccount {
            id: accounts::AccountId::from("cloud"),
            kind: crate::remote_accounts::RemoteAccountKind::Nextcloud,
            label: "Cloud".into(),
            host: "cloud.example".into(),
            health,
            files_mode: accounts::cloud_types::FilesMode::Sync,
            mount,
        }
    }

    fn a_mounted_remote_account_opens_even_offline_but_not_when_disabled() {
        let home = tempfile::tempdir().unwrap();
        let mount = home.path().join("Remote/Cloud");
        std::fs::create_dir_all(&mount).unwrap();
        let offline = remote_account(
            crate::remote_accounts::RemoteAccountHealth::Offline,
            Some(mount.clone()),
        );
        assert_eq!(
            remote_folder_availability(&offline),
            Availability::Available,
            "cached files stay readable and writes queue while offline"
        );
        assert!(matches!(
            remote_folder_availability(&remote_account(
                crate::remote_accounts::RemoteAccountHealth::Disabled,
                Some(mount),
            )),
            Availability::Unavailable { .. }
        ));
        assert!(matches!(
            remote_folder_availability(&remote_account(
                crate::remote_accounts::RemoteAccountHealth::Healthy,
                Some(home.path().join("Remote/Missing")),
            )),
            Availability::Unavailable { .. }
        ));
    }

    /// An account that needs signing in is a row in the places list that
    /// opens one short sheet: said once, with no subtitle and nothing about
    /// the vault, and one action that looks again.
    #[test]
    fn a_cloud_account_needing_sign_in_is_visible_and_says_what_to_do() {
        let account = remote_account(
            crate::remote_accounts::RemoteAccountHealth::NeedsAuthentication,
            None,
        );
        let mut w = FileExplorerWidget::new();
        w.debug_show_remote_account(&account, true);
        let ids: Vec<String> = w
            .current_places()
            .sections()
            .iter()
            .flat_map(|section| section.items.iter().map(|item| item.id.clone()))
            .collect();
        assert!(
            ids.iter().any(|id| id == "account:cloud"),
            "the account has a row even though nothing is mounted: {ids:?}"
        );
        let panel = w.collab_panel.as_ref().expect("the sheet is open");
        assert_eq!(panel.title, "Sign in to Cloud");
        assert!(panel.summary.is_empty(), "no subtitle under the heading");
        let text = panel.lines.join(" ");
        assert!(!text.to_lowercase().contains("vault"), "{text}");
        assert!(!text.contains("Remote account details"), "{text}");
        assert_eq!(
            w.collab_panel_action_label().as_deref(),
            Some("Check again")
        );
        assert_eq!(
            w.collab_panel_action_command(),
            Some(FileCommand::RecheckRemoteAccount("cloud".into()))
        );
        // Choosing the row opens the same sheet.
        w.close_collab_panel();
        assert!(w.apply_command(FileCommand::ShowRemoteAccount("cloud".into())));
        assert!(w.collab_panel.is_some());
    }

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

    /// Entry-level resource actions carry opaque ids and refuse stale, replayed
    /// or mistyped invocations — each phase on its own temp dir and widget.
    #[test]
    fn entry_resource_actions_are_opaque_and_fail_closed() {
        resource_open_refuses_a_deleted_or_replayed_entry_target();
        local_resource_actions_carry_only_opaque_ids_not_host_paths();
        entry_resource_uses_a_wire_safe_id_that_survives_a_rename();
        projected_availability_controls_are_discoverable_and_use_the_entry_route();
        nearby_receiver_actions_are_opaque_confirmed_and_current();
    }

    fn projected_availability_controls_are_discoverable_and_use_the_entry_route() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("shared.txt");
        std::fs::write(&path, b"x").unwrap();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = path;
        entry.availability = Some(fs_protocol::AvailabilityProjectionV1 {
            schema_version: 1,
            record_id: fs_protocol::FileRecordId(uuid::Uuid::from_u128(1)),
            current_version: Some(fs_protocol::FileVersionId(uuid::Uuid::from_u128(2))),
            residency: fs_protocol::Residency::Partial { resident_bytes: 1 },
            transfer: fs_protocol::ProjectedTransfer {
                phase: fs_protocol::TransferPhase::Idle,
                progress: None,
                last_error: None,
                last_error_at_ms: None,
                error_truncated: false,
            },
            policy: fs_protocol::AvailabilityPolicy::Automatic,
            policy_inherited: false,
            replicas: fs_protocol::ProjectedReplicaEvidence::Unknown,
            freshness: fs_protocol::ProjectedFreshness::default(),
        });
        let mut w = FileExplorerWidget::new();
        w.current_path = d.path().to_path_buf();
        w.entries = vec![entry];
        let mut doc = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(&w, &doc);
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
        let row = resource.id.as_str().to_owned();
        assert!(matches!(
            crate::files_app::published::invoke(
                &mut w,
                &mut doc,
                &row,
                "availability",
                &serde_json::json!({}),
            )
            .unwrap(),
            Some(ui::framework::Effected::Done(_))
        ));
        assert!(w.collab_panel.is_some(), "availability route opens details");
    }

    #[test]
    fn trash_and_archives_guard_destructive_actions() {
        trash_actions_require_an_explicit_irrevocable_confirmation();
        archives_browse_as_read_only_folders_and_extract_from_inside();
    }

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
            Some("Delete permanently")
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

    /// Every context/overlay menu surface, each phase on a fresh widget.
    #[test]
    fn context_menus_follow_selection_clipboard_location_and_templates() {
        super::open_with_tests::context_menu_opens_with_selection_aware_items();
        super::open_with_tests::empty_selection_menu_omits_item_actions();
        super::open_with_tests::the_folder_menu_holds_the_folder_actions();
        super::open_with_tests::menus_never_separate_a_lone_item();
        super::open_with_tests::copy_then_paste_menu_includes_paste();
        #[cfg(unix)]
        trash_view_menu_offers_restore_and_empty_not_move_to_trash();
        context_menu_exposes_the_sort_section_including_type();
        context_select_targets_the_clicked_row_not_a_stale_selection();
        context_menu_gates_new_file_on_templates_existing();
        disappearing_templates_do_not_leave_an_empty_flyout();
        large_text_view_menu_clamps_inside_the_live_window();
    }

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
            "Sort by name",
            "Sort by size",
            "Sort by type",
            "Sort by date",
        ] {
            assert!(
                labels.contains(&s.to_string()),
                "menu missing {s}: {labels:?}"
            );
        }
    }

    /// Listing, error copy/retry, history, double-click and scroll retention, each
    /// phase on its own temp dir and widget.
    #[test]
    fn navigation_listing_history_and_error_recovery() {
        state_surface_copy_names_the_folder_and_offers_what_can_help();
        retry_refresh_recovers_from_error_once_the_dir_exists();
        history_back_and_forward_traverse_visited_dirs();
        double_click_click_command_navigates_into_a_directory();
        a_populated_directory_lists_exactly_its_contents();
        scroll_position_survives_a_structural_rebuild_but_resets_on_navigation();
    }

    fn state_surface_copy_names_the_folder_and_offers_what_can_help() {
        let mut w = FileExplorerWidget::new();
        w.current_path = PathBuf::from("/workspace/projects/secret");
        let denied = std::io::Error::from_raw_os_error(13).to_string();
        w.error = Some(t_args(
            "files-directory-read-failed",
            &[arg("error", denied)],
        ));

        let (title, detail, has_action) = w.state_surface_copy().expect("error state copy");
        assert_eq!(
            title,
            t_args("files-load-error-title", &[arg("name", "secret")])
        );
        assert_eq!(detail, t("files-folder-error-permission"));
        assert!(has_action);
        assert_eq!(
            w.folder_recovery_command(),
            FileCommand::Up,
            "trying again can't fix a permission; going up can"
        );
        assert_eq!(w.folder_recovery_label(), t("files-open-enclosing"));

        let odd = std::io::Error::from_raw_os_error(5).to_string();
        w.error = Some(t_args("files-directory-read-failed", &[arg("error", odd)]));
        let (_, detail, _) = w.state_surface_copy().expect("error state copy");
        assert!(!detail.contains("os error"), "{detail}");
        assert_eq!(w.folder_recovery_command(), FileCommand::Refresh);
        assert_eq!(w.folder_recovery_label(), t("files-try-again"));
    }

    fn retry_refresh_recovers_from_error_once_the_dir_exists() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("appears-later");
        let mut w = FileExplorerWidget::new();
        w.navigate(target.clone());
        assert!(w.error_message().is_some(), "missing dir sets an error");
        // The "Try again" button pushes Refresh — still errors while missing.
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

    /// Rename (command, exported action and headless F2) plus the flows that drop
    /// into rename — each phase on its own temp dir and widget.
    #[test]
    fn rename_and_creation_flows_change_disk_names_locally() {
        rename_flow_renames_the_selected_file_on_disk();
        reseeding_the_rename_field_never_mutates_the_shared_document();
        rename_commit_is_a_noop_for_blank_or_unchanged_names();
        rename_cancel_leaves_the_file_untouched();
        exported_rename_changes_case_without_overwriting_another_entry();
        new_folder_creates_the_folder_and_opens_inline_rename();
        new_from_template_creates_the_file_and_opens_rename();
        super::headless_f2_renames_fixture_without_opening_a_file();
    }

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
        w.sync(&doc, &root);

        for i in 1..40 {
            w.set_rename_buffer(format!("report-{i:03}.txt"));
            w.sync(&doc, &root);
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

    fn new_folder_creates_the_folder_and_opens_inline_rename() {
        let d = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        assert!(w.apply_command(FileCommand::NewFolder));
        assert!(
            d.path().join("New folder").is_dir(),
            "folder actually created"
        );
        assert!(w.is_renaming(), "new folder drops straight into rename");
        assert_eq!(w.rename_buffer(), "New folder");
    }

    fn resource_open_refuses_a_deleted_or_replayed_entry_target() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("transient.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let mut doc = automerge::AutoCommit::new();
        let open = crate::effects::FilesEffect::OpenItem {
            item: crate::search_provider::entry_resource_id(&file),
        }
        .bound()
        .build_json(&serde_json::Map::new())
        .unwrap();

        std::fs::remove_file(&file).unwrap();
        w.refresh();
        let error = crate::files_app::published::start(
            &mut w,
            OperationKind::View,
            &open.type_id,
            &open.payload,
            &mut doc,
            &automerge::ROOT,
        )
        .expect_err("a stale entry action must be refused");
        assert!(error.contains("no longer available"));
    }

    fn local_resource_actions_carry_only_opaque_ids_not_host_paths() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("private-plan.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let resources = crate::files_app::published::tree(&w, &automerge::AutoCommit::new());
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
        let payload: serde_json::Value =
            serde_json::from_slice(&encoded.payload).expect("an effect payload");
        assert_eq!(payload["args"]["item"], entry.id.as_str());
        assert!(!String::from_utf8_lossy(&encoded.payload).contains("private-plan.txt"));
    }

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
        let catalogue = crate::files_app::published::tree(&w, &doc);
        let eager_entries = catalogue[0]
            .children
            .iter()
            .filter(|resource| resource.kind == "files.entry")
            .count();
        assert_eq!(eager_entries, MAX_EAGER_ENTRY_RESOURCES);

        let last = w.entries.last().expect("fixture has a final entry");
        let id = FileExplorerWidget::entry_resource_id(last);
        let resolved = w
            .published_row(&id)
            .expect("an exact current-folder entry resolves on demand");
        assert_eq!(resolved.id.as_str(), id);
        assert!(resolved.actions.iter().any(|action| action.id == "open"));
        assert!(resolved.actions.iter().any(|action| action.id == "copy"));
    }

    fn every_exported_local_file_handler_is_declared_for_the_broker() {
        // One published operation without a handler refuses the app's whole
        // surface, and a directory search then finds none of its actions.
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("notes.txt"), b"txt").unwrap();
        let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
        files.navigate(root.path().to_path_buf());
        let resources = crate::files_app::published::tree(&files, &automerge::AutoCommit::new());
        let registry = ui::tilo_actions::ActionRegistry::with(
            <crate::resource::FileExplorer as ui::tilo_resource::Resource>::register,
        );
        // The surface host answers the effect vocabulary beside the window's
        // own operations.
        let mut handlers = Widget::resource_operation_handlers(&files);
        handlers.insert(
            <crate::effects::FilesEffect as ui::tilo_resource::Effect>::OPERATION.to_owned(),
        );
        let violations = ui::tilo_actions::validate_surface(&resources, &registry, &handlers);
        assert!(violations.is_empty(), "{violations:?}");
    }

    /// An agent sorts a downloads folder with typed actions only: `go-to` an
    /// absolute folder, browse its entries, and `move-to`/`copy-to` another.
    #[test]
    fn go_to_move_to_and_copy_to_organise_files_by_absolute_path() {
        use ui::tilo_actions::{ActionAddress, door};
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("in/downloads");
        let images = root.path().join("out/images");
        let documents = root.path().join("out/documents");
        for dir in [&downloads, &images, &documents] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(downloads.join("holiday.jpg"), b"jpg").unwrap();
        std::fs::write(downloads.join("notes.txt"), b"txt").unwrap();
        let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
        let mut doc = automerge::AutoCommit::new();
        let invoke = |files: &mut FileExplorerWidget,
                      doc: &mut automerge::AutoCommit,
                      target: (String, &str),
                      arg: (&str, &Path)| {
            let (resource, action) = target;
            let resources = crate::files_app::published::tree(&*files, doc);
            let arguments = serde_json::Map::from_iter([(
                arg.0.to_string(),
                serde_json::Value::String(arg.1.to_string_lossy().into_owned()),
            )]);
            let encoded = door::encode_json(
                &resources,
                &ActionAddress::new(resource, action),
                &arguments,
            )
            .unwrap();
            let kind = if action == "go-to" {
                OperationKind::View
            } else {
                OperationKind::Effect
            };
            crate::files_app::published::start(
                files,
                kind,
                &encoded.type_id,
                &encoded.payload,
                doc,
                &automerge::ROOT,
            )
        };

        let start = (
            <crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND.to_owned(),
            "go-to",
        );
        invoke(&mut files, &mut doc, start, ("path", &downloads)).unwrap();
        assert_eq!(files.current_path, downloads);
        let folder =
            <crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND.to_owned();
        let relative = invoke(
            &mut files,
            &mut doc,
            (folder, "go-to"),
            ("path", Path::new("in")),
        );
        assert!(relative.unwrap_err().contains("absolute"));

        let browsed = crate::files_app::published::tree(&files, &doc).remove(0);
        let holiday = crate::search_provider::entry_resource_id(&downloads.join("holiday.jpg"));
        let notes = crate::search_provider::entry_resource_id(&downloads.join("notes.txt"));
        let children: Vec<&str> = browsed.children.iter().map(|c| c.id.as_str()).collect();
        assert!(children.contains(&holiday.as_str()) && children.contains(&notes.as_str()));

        invoke(
            &mut files,
            &mut doc,
            (notes.clone(), "copy-to"),
            ("folder", &documents),
        )
        .unwrap();
        assert!(documents.join("notes.txt").exists() && downloads.join("notes.txt").exists());
        assert!(images.is_dir() && downloads.join("holiday.jpg").exists() && !holiday.is_empty());
        let nowhere = root.path().join("nope");
        let missing = invoke(
            &mut files,
            &mut doc,
            (notes, "copy-to"),
            ("folder", &nowhere),
        );
        assert!(missing.unwrap_err().contains("is not an existing folder"));
    }

    /// `go-to` and `open` with a path show the folder and answer with its
    /// new resource id and entries, so the caller needs no query to act.
    #[test]
    fn showing_a_folder_answers_with_its_resource_and_entries() {
        use ui::tilo_actions::{ActionAddress, door};
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("in/downloads");
        std::fs::create_dir_all(downloads.join("old")).unwrap();
        for index in 0..70 {
            std::fs::write(downloads.join(format!("f{index:02}.txt")), "x").unwrap();
        }
        let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
        files.navigate(root.path().into());
        let mut doc = automerge::AutoCommit::new();
        let mut show = |action: &str, path: &Path| {
            let resources = crate::files_app::published::tree(&files, &doc);
            let folder = <crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND;
            let encoded = door::encode_json(
                &resources,
                &ActionAddress::new(folder, action),
                serde_json::json!({"path": path}).as_object().unwrap(),
            )
            .unwrap();
            let started = crate::files_app::published::start(
                &mut files,
                OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &mut doc,
                &automerge::ROOT,
            );
            match started {
                Ok(Some(ui::framework::Effected::Done(output))) => output.value,
                other => panic!("{action} completes at once: {other:?}"),
            }
        };

        let opened = show("open-folder", &downloads).unwrap();
        assert_eq!(
            opened["resource"],
            <crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND
        );
        assert_eq!(opened["path"], downloads.display().to_string());
        let entries = opened["entries"].as_array().unwrap();
        assert_eq!(entries.len(), MAX_ANSWERED_ENTRIES);
        assert_eq!(entries[0], "old/");
        assert_eq!(entries[1], "f00.txt");
        assert_eq!(opened["more"], 71 - MAX_ANSWERED_ENTRIES);

        let old = downloads.join("old");
        let went = show("go-to", &old).unwrap();
        assert_eq!(
            went["resource"],
            <crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND
        );
        assert_eq!(went["entries"], serde_json::json!([]));
        assert!(went.get("more").is_none());
    }

    fn exported_rename_changes_case_without_overwriting_another_entry() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("report.txt");
        std::fs::write(&original, b"source content").unwrap();
        let occupied = directory.path().join("occupied.txt");
        std::fs::write(&occupied, b"keep occupied content").unwrap();
        let mut files = FileExplorerWidget::with_persistence(None, Some(directory.path().into()));
        files.navigate(directory.path().into());
        // The rename editor's commit queues the same operation an agent's
        // `rename` does; a window no framework hosts carries it out at once,
        // keeping the editor open when it fails.
        let mut invoke_rename = |source: &Path, name: &str| -> Result<bool, String> {
            files.refresh();
            let index = files
                .displayed_entries()
                .iter()
                .position(|entry| entry.path == source)
                .ok_or_else(|| "not listed".to_string())?;
            files.selection.select_only(index);
            files.begin_rename();
            files.set_rename_buffer(name.into());
            files.commit_rename();
            if files.is_renaming() {
                files.cancel_rename();
                return Err(files.status.clone().unwrap_or_default());
            }
            Ok(true)
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

    fn entry_resource_uses_a_wire_safe_id_that_survives_a_rename() {
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
        let mut doc = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(&w, &doc);
        let entry = resources[0]
            .children
            .iter()
            .find(|resource| {
                resource.id.as_str() == crate::search_provider::record_resource_id(record_id)
            })
            .expect("a collaborative file uses its record UUID as the resource identity");
        assert_eq!(entry.kind, "files.record");
        let address = ActionAddress::new(entry.id.as_str(), "copy");
        assert_eq!(
            ActionAddress::parse(&address.encode()),
            Some(address.clone())
        );
        assert!(door::resolve(&resources, &address).is_some());
        // Renamed in place, the file keeps its record identity.
        w.selection.select_only(0);
        w.begin_rename();
        w.set_rename_buffer("new.txt".into());
        w.commit_rename();
        let _ = &mut doc;
        assert!(!old.exists());
        let renamed = d.path().join("new.txt");
        assert!(renamed.exists());
        // A mount refresh presents a new path but the same daemon-provided
        // record identity, so the old action address resolves against it.
        w.entries[0].path = renamed;
        w.entries[0].name = "new.txt".into();
        w.entries[0].record_id = Some(*record_id.as_bytes());
        let refreshed = crate::files_app::published::tree(&w, &doc);
        assert!(
            door::resolve(&refreshed, &address).is_some(),
            "the record action address survives a path rename and resolves its fresh path"
        );
    }

    fn exported_background_operations_have_valid_job_contracts() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("shared.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let resources = crate::files_app::published::tree(&w, &automerge::AutoCommit::new());
        for action in resources.iter().flat_map(|resource| {
            resource.actions.iter().chain(
                resource
                    .children
                    .iter()
                    .flat_map(|child| child.actions.iter()),
            )
        }) {
            if matches!(action.contract.execution, ExecutionMode::Background { .. }) {
                // Inspection and recovery answer with declared JSON receipts.
                // Every existing void job must still declare Job.
                let expected = if [
                    crate::effects::FilesEffect::CLIPBOARD_STATUS.id,
                    crate::effects::FilesEffect::RECOVER_CLIPBOARD_TRANSFERS.id,
                ]
                .contains(&action.id.as_str())
                {
                    ResultSpec::Json {
                        schema: serde_json::json!({}),
                    }
                } else {
                    ResultSpec::Job
                };
                assert_eq!(action.contract.result, expected, "{}", action.id);
                assert!(
                    action.contract.validate_declaration().is_empty(),
                    "{}",
                    action.id
                );
            }
        }
    }

    /// Each transfer phase uses its own temp tree and widget.
    #[test]
    fn clipboard_copy_cut_paste_moves_files_and_retries_failures() {
        copy_then_paste_duplicates_the_file_into_the_current_dir();
        cut_then_paste_moves_the_file_into_the_current_dir();
        partial_cut_paste_retains_only_the_sources_that_need_retrying();
        #[cfg(unix)]
        exported_paste_preserves_dangling_destination_links();
        exported_cut_paste_reports_destination_failure_and_keeps_retryable_source();
    }

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
        let mut doc = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(files, &doc);
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
        crate::files_app::published::start(
            files,
            OperationKind::Effect,
            &encoded.type_id,
            &encoded.payload,
            &mut doc,
            &automerge::ROOT,
        )
        .map(|started| matches!(started, Some(ui::framework::Effected::Done(_))))
    }

    #[cfg(unix)]
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

    /// A hermetic widget whose recent files and previews live in a throwaway
    /// dir, so tests never touch the user's real `~/.config`.
    fn hermetic() -> (FileExplorerWidget, tempfile::TempDir) {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path()),
            Some(PathBuf::from("/home/tester")),
        );
        (w, cfg)
    }

    #[test]
    #[cfg(unix)]
    fn airdrop_file_action_requires_host_availability_and_cannot_submit_trust() {
        use ui::collaboration::{
            CollabEvent, CollabRequest, NearbyTransferCommand, NearbyTransferReply,
        };
        let (mut widget, config) = hermetic();
        let source = config.path().join("report.txt");
        std::fs::write(&source, b"approved source").unwrap();
        widget.navigate(config.path().to_path_buf());
        let mut document = automerge::AutoCommit::new();
        let address = ui::tilo_actions::ActionAddress::new(
            crate::search_provider::entry_resource_id(&source),
            "send-airdrop",
        );
        let resources = crate::files_app::published::tree(&widget, &document);
        assert!(ui::tilo_actions::door::resolve(&resources, &address).is_none());
        widget
            .nearby_send
            .handle_event(&CollabEvent::NearbyTransferChanged);
        let request_id = widget
            .nearby_send
            .take_requests()
            .iter()
            .find_map(|request| match request {
                CollabRequest::NearbyTransferCommand {
                    request_id,
                    command: NearbyTransferCommand::AirDropAvailability,
                    ..
                } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        widget
            .nearby_send
            .handle_event(&CollabEvent::NearbyTransferCommandResult {
                request_id,
                result: Ok(NearbyTransferReply::AirDropAvailability { available: true }),
            });
        let resources = crate::files_app::published::tree(&widget, &document);
        let action = ui::tilo_actions::door::resolve(&resources, &address)
            .expect("lab host publishes file action");
        assert_eq!(
            action
                .params()
                .iter()
                .map(|param| &*param.name)
                .collect::<Vec<_>>(),
            ["address", "port"]
        );
        let mut args = serde_json::Map::new();
        args.insert("address".into(), "127.0.0.1".into());
        args.insert("port".into(), 443.into());
        let encoded = action.build_json(&args).unwrap();
        crate::files_app::published::start(
            &mut widget,
            action.kind,
            &encoded.type_id,
            &encoded.payload,
            &mut document,
            &automerge::ROOT,
        )
        .unwrap();
        let requests = widget.nearby_send.take_requests();
        assert!(
            matches!(requests.as_slice(), [CollabRequest::NearbyTransferCommand {
            command: NearbyTransferCommand::SendAirDrop { source_path, address, port: 443 }, ..
        }] if source_path == source.as_os_str().as_encoded_bytes() && address == "127.0.0.1")
        );
        // Retaining an encoded action does not retain its source capability.
        std::fs::remove_file(&source).unwrap();
        widget.refresh();
        assert!(
            crate::files_app::published::start(
                &mut widget,
                action.kind,
                &encoded.type_id,
                &encoded.payload,
                &mut document,
                &automerge::ROOT
            )
            .is_err()
        );
        assert!(widget.nearby_send.take_requests().is_empty());
    }

    #[test]
    fn nearby_status_action_reports_progress_and_terminal_phases_without_authority() {
        use ui::collaboration::{
            NearbyTransferDirection, NearbyTransferHandle, NearbyTransferPhase,
            NearbyTransferSnapshot, NearbyTransferStatus,
        };
        use ui::tilo_actions::ActionInput;

        let transfer = NearbyTransferHandle::from_issued_bytes([0x62; 16]);
        let (mut widget, _config) = hermetic();
        let mut document = automerge::AutoCommit::new();
        for (phase, bytes) in [
            (NearbyTransferPhase::AwaitingConsent, 0),
            (NearbyTransferPhase::Transferring, 8),
            (NearbyTransferPhase::Completed, 16),
            (NearbyTransferPhase::Failed, 8),
            (NearbyTransferPhase::Cancelled, 8),
        ] {
            let expected = NearbyTransferStatus {
                handle: transfer,
                name: "report.txt".into(),
                device_label: "Verified fixture".into(),
                direction: NearbyTransferDirection::Sending,
                phase,
                transferred_bytes: bytes,
                total_bytes: 16,
            };
            widget
                .nearby_send
                .install_snapshot_for_test(NearbyTransferSnapshot {
                    transfers: vec![expected.clone()],
                    ..Default::default()
                });
            let rows = crate::files_app::published::tree(&widget, &document);
            let resource = rows[0]
                .children
                .iter()
                .find(|resource| resource.kind == "files.nearby-transfer")
                .expect("terminal transfers remain semantic resources");
            let status = resource
                .actions
                .iter()
                .find(|action| action.id == "status")
                .expect("a read-only status action exists in every phase");
            assert_eq!(status.contract.kind, ActionKind::Read);
            assert_eq!(status.contract.confirmation, ConfirmationPolicy::None);
            let ActionInput::Immediate(encoded) = &status.input else {
                panic!("status uses the app's opaque row payload")
            };
            let Some(ui::framework::Effected::Done(output)) = crate::files_app::published::start(
                &mut widget,
                status.kind,
                &encoded.type_id,
                &encoded.payload,
                &mut document,
                &automerge::ROOT,
            )
            .expect("read status through the production effect door") else {
                panic!("status must return an immediate structured answer")
            };
            assert_eq!(
                output.value,
                Some(serde_json::json!({"transfer": expected}))
            );
            assert!(
                widget.nearby_send.take_requests().is_empty(),
                "inspection grants no authority or network work"
            );
            widget
                .nearby_send
                .install_snapshot_for_test(NearbyTransferSnapshot::default());
            assert!(
                crate::files_app::published::start(
                    &mut widget,
                    status.kind,
                    &encoded.type_id,
                    &encoded.payload,
                    &mut document,
                    &automerge::ROOT,
                )
                .expect_err("an expired row cannot inspect another transfer")
                .contains("no longer available")
            );
            assert!(widget.nearby_send.take_requests().is_empty());
        }
    }

    #[test]
    fn nearby_recovered_receive_decline_is_available_without_a_destination() {
        use ui::collaboration::{
            CollabRequest, NearbyTransferCommand, NearbyTransferDirection, NearbyTransferHandle,
            NearbyTransferPhase, NearbyTransferSnapshot, NearbyTransferStatus,
        };
        use ui::tilo_actions::ActionInput;

        let transfer = NearbyTransferHandle::from_issued_bytes([0x61; 16]);
        let (mut widget, _config) = hermetic();
        widget
            .nearby_send
            .install_snapshot_for_test(NearbyTransferSnapshot {
                transfers: vec![NearbyTransferStatus {
                    handle: transfer,
                    name: "Interrupted report".into(),
                    device_label: "Offline laptop".into(),
                    direction: NearbyTransferDirection::Receiving,
                    phase: NearbyTransferPhase::AwaitingDestination,
                    transferred_bytes: 8,
                    total_bytes: 16,
                }],
                ..Default::default()
            });
        let rows = crate::files_app::published::tree(&widget, &automerge::AutoCommit::new());
        let resource = rows[0]
            .children
            .iter()
            .find(|resource| resource.kind == "files.nearby-transfer")
            .expect("recoverable receive remains a semantic resource");
        let decline = resource
            .actions
            .iter()
            .find(|action| action.id == "decline")
            .expect("decline needs no destination grant");
        assert_eq!(decline.contract.confirmation, ConfirmationPolicy::Confirm);
        let ActionInput::Immediate(encoded) = &decline.input else {
            panic!("decline must use the existing opaque action payload")
        };
        let mut document = automerge::AutoCommit::new();
        crate::files_app::published::start(
            &mut widget,
            OperationKind::Effect,
            &encoded.type_id,
            &encoded.payload,
            &mut document,
            &automerge::ROOT,
        )
        .expect("execute recovered receive decline");
        assert!(matches!(widget.nearby_send.take_requests().as_slice(), [
            CollabRequest::NearbyTransferCommand {
                command: NearbyTransferCommand::Decide {
                    handle, accept: false, destination: None, remember_for_session: false,
                }, ..
            }
        ] if *handle == transfer));
    }

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
        let rows = crate::files_app::published::tree(&widget, &automerge::AutoCommit::new());
        let transfer_resource = rows[0]
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
        let mut document = automerge::AutoCommit::new();
        assert!(
            crate::files_app::published::start(
                &mut widget,
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
                &mut document,
                &automerge::ROOT
            )
            .expect_err("stale receiver decision must fail closed")
            .contains("no longer available")
        );
        assert!(widget.nearby_send.take_requests().is_empty());
    }

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

    /// Favourite commands edit the sidebar, which follows the localized
    /// user folders. (The stored places are the resource's: see
    /// `resource::tests`.)
    #[test]
    fn favourites_edit_the_sidebar_and_follow_localized_user_dirs() {
        favorite_commands_add_reorder_and_remove();
        favourites_follow_localized_xdg_user_dirs();
    }

    fn favorite_commands_add_reorder_and_remove() {
        let cfg = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::with_persistence(
            Some(cfg.path()),
            Some(PathBuf::from("/home/tester")),
        );
        let sub = PathBuf::from("/home/tester/Projects/tilo");
        let before = w.favorites().len();
        w.apply_command(FileCommand::AddFavorite(sub.clone()));
        assert_eq!(w.favorites().len(), before + 1, "added one favourite");
        assert_eq!(w.favorites().last().unwrap().path, sub);

        // Reorder: drag the new favourite (last) up to index 1.
        let last = w.favorites().len() - 1;
        w.apply_command(FileCommand::ReorderFavorite { from: last, to: 1 });
        assert_eq!(w.favorites()[1].path, sub, "reorder moved it to slot 1");

        w.apply_command(FileCommand::RemoveFavorite(sub.clone()));
        assert!(!w.favorites().iter().any(|f| f.path == sub));
    }

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
        let w =
            FileExplorerWidget::with_persistence_dirs(Some(cfg.path()), Some(home.clone()), dirs);
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

    fn new_from_template_creates_the_file_and_opens_rename() {
        let d = tempfile::tempdir().unwrap();
        let tmpl = d.path().join("Doc.md");
        std::fs::write(&tmpl, b"template body").unwrap();
        let work = d.path().join("work");
        std::fs::create_dir(&work).unwrap();

        let (mut w, _cfg) = hermetic();
        w.templates_dir = Some(d.path().to_path_buf());
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

    fn context_menu_gates_new_file_on_templates_existing() {
        // The "New File ▸" entry appears iff the Templates dir has files, and
        // "New folder" is always present. Templates are injected so the check is
        // hermetic (no reliance on the ambient Templates dir).
        let (w, _cfg) = hermetic();
        let has_new_file = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label.starts_with("New file"))
        };
        let has_new_folder = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label == "New folder")
        };

        // Empty Templates dir → no "New File", but "New folder" stays.
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

    fn disappearing_templates_do_not_leave_an_empty_flyout() {
        let (mut w, _cfg) = hermetic();
        // A catalogue can be emptied after the parent menu opens. The flyout
        // must close instead of painting an empty, focusable surface.
        w.open_templates_submenu(Point::new(80.0, 80.0));
        assert!(!w.submenu.is_open());
        assert!(w.submenu_commands.is_empty());
    }

    /// Keep-offline, free-space, policy and retry/details affordances, each phase
    /// on its own temp files and widget.
    #[test]
    fn availability_policy_commands_write_only_eligible_records() {
        #[cfg(unix)]
        collaborative_selection_offers_and_applies_keep_offline();
        #[cfg(unix)]
        free_local_space_is_offered_and_sent_only_for_eligible_local_files();
        #[cfg(unix)]
        mixed_selection_exposes_both_storage_policies_and_writes_only_eligible_records();
        #[cfg(unix)]
        inherited_pin_is_explained_but_not_offered_as_a_local_toggle();
        failed_projected_file_offers_retry_and_availability_details();
    }

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
        xattr::set(&file, fs_protocol::AVAILABILITY_XATTR, &availability).unwrap();

        let mut w = FileExplorerWidget::new();
        w.current_path = dir.path().to_path_buf();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = file.clone();
        entry.availability = crate::model::read_availability(&file);
        w.entries = vec![entry];
        w.selection.select_only(0);
        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Keep offline" && *command == Some(FileCommand::KeepOffline)
        }));
        assert!(w.apply_command(FileCommand::KeepOffline));
        assert_eq!(
            xattr::get(&file, fs_protocol::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice())
        );
    }

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
            fs_protocol::AVAILABILITY_XATTR,
            &availability(serde_json::json!({"state": "resident", "bytes": 5})),
        )
        .unwrap();
        xattr::set(
            &remote,
            fs_protocol::AVAILABILITY_XATTR,
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
                    label == "Free local space" && *command == Some(FileCommand::FreeLocalSpace)
                })
        );
        assert!(widget.apply_command(FileCommand::FreeLocalSpace));
        assert_eq!(
            xattr::get(&local, fs_protocol::AVAILABILITY_CONTROL_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"evict".as_slice())
        );
        assert_eq!(
            xattr::get(&remote, fs_protocol::AVAILABILITY_CONTROL_XATTR).unwrap(),
            None
        );
    }

    #[cfg(unix)]
    fn scheduled_metadata_refresh_preserves_record_selection_and_scroll() {
        let dir = tempfile::tempdir().unwrap();
        // Production discovers this from the mounted workspace root. Mark the
        // isolated fixture as a collaborative root so the listing reads the
        // record/sync xattrs whose refresh behavior this test exercises.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ROOTS", dir.path());
        let before = dir.path().join("before.txt");
        let after = dir.path().join("after.txt");
        std::fs::write(&before, b"shared").unwrap();
        let record = uuid::Uuid::from_u128(0x5151);
        xattr::set(&before, fs_protocol::RECORD_ID_XATTR, record.as_bytes()).unwrap();
        xattr::set(&before, fs_protocol::SYNC_XATTR, b"projected").unwrap();

        let mut widget = FileExplorerWidget::new();
        widget.load(dir.path().to_path_buf());
        // A workspace mount is FUSE: its watch reports local edits only, and
        // remote ones (renames, sync state) are found by the poll.
        widget.directory_watch.assume_partial_coverage();
        widget.selection.select_only(0);
        widget.scroll_offset.set((0.0, 137.0));
        std::fs::rename(&before, &after).unwrap();
        xattr::set(&after, fs_protocol::SYNC_XATTR, b"local").unwrap();
        widget.next_metadata_refresh = Some(ui::time::now());

        assert!(widget.refresh_metadata_if_due(ui::time::now()));
        assert_eq!(widget.entries[0].path, after);
        assert_eq!(
            widget.entries[0].sync_state,
            Some(fs_protocol::SyncState::Local)
        );
        assert_eq!(widget.selection.selected(), vec![0]);
        assert_eq!(widget.selection.primary(), Some(0));
        assert_eq!(widget.scroll_offset.get(), (0.0, 137.0));
    }

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
            fs_protocol::AVAILABILITY_XATTR,
            &availability("automatic", false),
        )
        .unwrap();
        xattr::set(
            &pinned,
            fs_protocol::AVAILABILITY_XATTR,
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
            label == "Keep offline" && *command == Some(FileCommand::KeepOffline)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Use automatic storage" && *command == Some(FileCommand::RemoveOfflinePin)
        }));

        w.apply_command(FileCommand::KeepOffline);
        assert_eq!(
            xattr::get(&automatic, fs_protocol::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice())
        );
        assert_eq!(xattr::get(&pinned, fs_protocol::PIN_XATTR).unwrap(), None);
        assert_eq!(xattr::get(&ordinary, fs_protocol::PIN_XATTR).unwrap(), None);

        // `refresh` deliberately treats this temp directory as an ordinary
        // non-workspace path. Restore the directly-read projections so the
        // second half can exercise the opposite command independently.
        for entry in &mut w.entries {
            entry.availability = crate::model::read_availability(&entry.path);
        }
        w.selection.select_all(w.entries.len());
        w.apply_command(FileCommand::RemoveOfflinePin);
        assert_eq!(
            xattr::get(&pinned, fs_protocol::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"automatic".as_slice())
        );
        assert_eq!(
            xattr::get(&automatic, fs_protocol::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice()),
            "the opposite policy action must not rewrite an automatic record"
        );
        assert_eq!(xattr::get(&ordinary, fs_protocol::PIN_XATTR).unwrap(), None);
    }

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
        xattr::set(&file, fs_protocol::AVAILABILITY_XATTR, &availability).unwrap();

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
                label == "Pinned by parent folder" && command.is_none()
            })
        );
        assert!(
            !spec
                .iter()
                .any(|(_, command)| { *command == Some(FileCommand::RemoveOfflinePin) })
        );
        w.apply_command(FileCommand::RemoveOfflinePin);
        assert_eq!(xattr::get(&file, fs_protocol::PIN_XATTR).unwrap(), None);
    }

    fn failed_projected_file_offers_retry_and_availability_details() {
        let mut entry = ent("offline.txt", false, "text/plain");
        entry.availability = fs_protocol::AvailabilityProjectionV1::parse_xattr(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_version": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
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
        )
        .ok();
        let mut w = FileExplorerWidget::new();
        w.entries = vec![entry];
        w.selection.select_only(0);

        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Retry download" && *command == Some(FileCommand::RetryTransfer)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Availability…" && *command == Some(FileCommand::ShowAvailability)
        }));

        w.apply_command(FileCommand::ShowAvailability);
        let panel = w.collab_panel.as_ref().expect("availability panel");
        assert!(panel.summary.contains("Not stored"));
        assert!(panel.lines.iter().any(|line| line.contains("other device")));
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("No available replica"))
        );
    }

    /// Sharing, version history and governed-identity surfaces, each phase with
    /// its own fixture (the share round trip owns its socket and wake scope).
    #[test]
    fn sharing_history_and_identity_surfaces_use_governed_identity() {
        authority_sharing_and_history_open_real_detail_panels();
        history_activity_intent_preserves_selected_version_and_signed_identity();
        #[cfg(unix)]
        named_recipient_menu_uses_identity_label_but_commands_the_root_key();
        running_files_reloads_new_renamed_removed_and_expired_governed_devices();
        #[cfg(unix)]
        share_composer_authority_round_trip_never_blocks_the_ui_command();
        named_share_status_distinguishes_pending_from_delivered();
        previous_immutable_version_is_an_explicit_restore_command();
        #[cfg(unix)]
        owner_context_menu_revokes_each_exact_active_grant();
    }

    fn authority_sharing_and_history_open_real_detail_panels() {
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.record_id =
            Some(*uuid::Uuid::from_u128(0xb0d4_e23f_c7cb_4ded_a427_b32c_f9da_9341).as_bytes());
        entry.rich_metadata = fs_protocol::MetadataProjectionV1::parse_xattr(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "properties": {"schema_version": 1, "image_width": 1920,
                    "image_height": 1080, "duration_ms": 65000,
                    "page_count": 4, "codec": "h264", "container": "mp4"}
            }))
            .unwrap(),
        )
        .ok();
        entry.share = fs_protocol::ShareProjectionV1::parse_xattr(
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
        )
        .ok();
        entry.history = fs_protocol::HistoryProjectionV1::parse_xattr(
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
        )
        .ok();
        let mut w = FileExplorerWidget::new();
        w.entries = vec![entry];
        w.selection.select_only(0);
        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Sharing and access…" && *command == Some(FileCommand::ShowSharing)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Version history…" && *command == Some(FileCommand::ShowHistory)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Properties…" && *command == Some(FileCommand::ShowProperties)
        }));
        w.apply_command(FileCommand::ShowProperties);
        let panel = w.collab_panel.as_ref().unwrap();
        assert!(panel.title.contains("Properties"));
        let internal = |line: &String| line.contains("Collaborative record");
        assert!(!panel.lines.iter().any(internal), "{:?}", panel.lines);
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
        w.apply_command(FileCommand::CloseCollabPanel);
        assert!(w.collab_panel.is_none());
    }

    /// A version an agent's own device signed reaches Workspace Activity as
    /// the agent's, on behalf of its person, as the file's History names it.
    #[test]
    fn history_activity_names_an_agents_version_as_the_agent_for_its_person() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let person = root.verifying_key().to_bytes();
        let agent_key = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: person,
            device_public_key: agent_key.verifying_key().to_bytes(),
            device_label: "Agent: Scribe".into(),
            capabilities: tilo_user_identity::CertificationPolicy::agent().ceiling,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
        let device = certificate.device_public_key;
        let version = "c0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let record = "c0d4e23f-c7cb-4ded-a427-b32cf9da9341";
        let mut entry = ent("report.txt", false, "text/plain");
        entry.history = fs_protocol::HistoryProjectionV1::parse_xattr(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "record_id": record, "total_versions": 1,
                "versions_truncated": false, "heads": [version],
                "versions": [
                    {"id": version, "record_id": record, "parents": [],
                     "actor": {"person": person.to_vec(), "device": device.to_vec()},
                     "timestamp_ms": 9, "content_hash": vec![3; 32], "metadata_hash": vec![4; 32], "provenance": "Edit"}
                ], "conflicts_truncated": false, "conflicts": []
            }))
            .unwrap(),
        ).ok();
        let mut w = FileExplorerWidget::new();
        w.trusted_humans = crate::identity_directory::TrustedHumanDirectory::from_projection_v2(
            tilo_user_identity::TrustedIdentityProjectionV2 {
                schema_version: tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
                humans: vec![tilo_user_identity::GovernedHumanIdentityV2 {
                    root_public_key: person,
                    display_name: "Ada Lovelace".to_string(),
                    devices: vec![tilo_user_identity::GovernedDeviceIdentityV2 {
                        certificate,
                        agent: Some(tilo_user_identity::GovernedAgentIdentityV2 {
                            agent_id: "scribe".into(),
                            label: "Scribe".into(),
                            color: [10, 20, 30],
                        }),
                    }],
                }],
            },
        )
        .unwrap();
        w.entries = vec![entry];
        w.selection.select_only(0);
        let selected = fs_protocol::FileVersionId(uuid::Uuid::parse_str(version).unwrap());
        w.apply_command(FileCommand::ShowInWorkspaceActivity {
            record_id: record.into(),
            version_id: selected,
        });
        let Some(ui::widget::AppHostIntent::ShowWorkspaceActivity {
            entry: Some(entry), ..
        }) = w.take_host_intent()
        else {
            panic!("Files hands the selected version to Workspace Activity");
        };
        assert_eq!(entry.author.name, "Scribe · Agent, for Ada Lovelace");
        assert_eq!(entry.author.relation, ui::widget::AuthorRelation::Software);
        let attribution = entry.author.attribution.expect("who acted for whom");
        assert_eq!(attribution.actor, "agent:scribe");
        assert_eq!(attribution.actor_name, "Scribe");
        assert_eq!(attribution.on_behalf_of, Some(hex::encode(person)));
        assert_eq!(
            attribution.on_behalf_of_name.as_deref(),
            Some("Ada Lovelace")
        );
    }

    fn history_activity_intent_preserves_selected_version_and_signed_identity() {
        let older = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let record = "b0d4e23f-c7cb-4ded-a427-b32cf9da9341";
        let mut entry = ent("report.txt", false, "text/plain");
        entry.history = fs_protocol::HistoryProjectionV1::parse_xattr(
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
        ).ok();
        let mut w = FileExplorerWidget::new();
        w.entries = vec![entry];
        w.selection.select_only(0);
        w.apply_command(FileCommand::ShowHistory);
        let selected = fs_protocol::FileVersionId(uuid::Uuid::parse_str(older).unwrap());
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
    #[cfg(unix)]
    fn named_recipient_menu_uses_identity_label_but_commands_the_root_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        std::fs::write(&file, b"report").unwrap();
        xattr::set(
            dir.path(),
            fs_protocol::SHARE_CONTROL_XATTR,
            b"/run/tilo/app/tilo-fs-share.sock",
        )
        .unwrap();
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
        let person_bytes = root_key.verifying_key().to_bytes();
        let person = fs_protocol::Ed25519PublicKey::from_bytes(person_bytes);
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
        let device =
            fs_protocol::Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes());
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
                    devices: vec![tilo_user_identity::GovernedDeviceIdentityV2 {
                        certificate,
                        agent: None,
                    }],
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
                            permission: fs_protocol::FilePermission::Edit,
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
                            permission: fs_protocol::FilePermission::Edit,
                            expiry: crate::sharing::ShareExpiryChoice::Month,
                            link_uses: Some(crate::sharing::LinkUseChoice::Bounded(10)),
                            recipient_devices: Vec::new(),
                        },
                    ))
        }));

        w.entries[0].share = Some(fs_protocol::ShareProjectionV1 {
            schema_version: 1,
            record_id: fs_protocol::FileRecordId(uuid::Uuid::from_u128(82)),
            observed_at_ms: 1,
            grants: vec![fs_protocol::ShareGrantProjectionV1 {
                grant_id: uuid::Uuid::from_u128(83),
                subject: fs_protocol::ShareSubjectProjection::Person { person },
                permission: fs_protocol::FilePermission::Edit,
                expires_at_ms: None,
                revoked: false,
                expired: false,
                used: false,
                exhausted: false,
                status: fs_protocol::ShareGrantStatus::Active,
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
                    tilo_user_identity::GovernedDeviceIdentityV2 {
                        certificate,
                        agent: None,
                    }
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
        let person = fs_protocol::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes());
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

    #[cfg(unix)]
    fn collaborative_search_activation_resolves_the_latest_path_by_record_id() {
        let dir = tempfile::tempdir().unwrap();
        let record_id = uuid::Uuid::from_u128(0xabc);
        let nested = dir.path().join("nested");
        let file = nested.join("renamed.txt");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&file, b"hello").unwrap();
        xattr::set(&file, fs_protocol::RECORD_ID_XATTR, record_id.as_bytes()).unwrap();
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
            fs_protocol::projection::RECORD_INDEX_XATTR,
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

        let source = ui::framework::testing::search_source::<crate::resource::FileExplorer>(
            widget.search_entries_shown(),
        );
        // Find the collaborative hit by its stable record anchor rather than
        // by position or a developer's local account settings.
        let hit = ui::search::SearchProvider::search(&source, "renamed")
            .into_iter()
            .find(|result| {
                result.action_id == crate::search_provider::record_resource_id(record_id)
            })
            .expect("collaborative record is searchable");
        let row = widget
            .published_row(&hit.anchor())
            .expect("late record materializes exactly for this anchor");
        assert!(
            row.actions
                .iter()
                .any(|spec| spec.id == crate::search_provider::REVEAL_ACTION),
            "the anchored row reveals the record"
        );
        widget
            .open_anchor(&hit.anchor())
            .expect("opening the anchor reveals the record");
        assert_eq!(widget.current_path, nested);
        assert_eq!(widget.selection.primary(), Some(0));
        assert_eq!(widget.entries[0].path, file);
    }

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
            fs_protocol::SHARE_CONTROL_XATTR,
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
            let response = fs_protocol::ShareControlResponseV1 {
                protocol_version: fs_protocol::SHARE_CONTROL_VERSION,
                result: fs_protocol::ShareControlResultV1::BearerCreatedV2 {
                    uri: "tilo-file://bounded".into(),
                },
            };
            let response = fs_protocol::canonical::canonical_bytes(&response).unwrap();
            stream
                .write_all(&(response.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&response).unwrap();
        });

        // Built under a bound wake scope, as the app factory builds it, so
        // the share outcome reaches the event loop.
        let mut scope = ui::WakeScope::new();
        let (woke, wakes) = std::sync::mpsc::channel();
        scope
            .bind(move |_| {
                let _ = woke.send(());
            })
            .unwrap();
        let _entry = scope.enter();
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
                permission: fs_protocol::FilePermission::Edit,
                expiry: crate::sharing::ShareExpiryChoice::Month,
                link_uses: Some(crate::sharing::LinkUseChoice::Bounded(10)),
                recipient_devices: Vec::new(),
            },
        )));
        // The daemon has the request but deliberately cannot reply yet. Reaching
        // this receive proves the UI command returned independently of I/O.
        seen_rx.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert!(widget.share_pending_path.is_some());
        release_tx.send(()).unwrap();
        server.join().unwrap();
        wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        let doc = automerge::AutoCommit::new();
        assert!(widget.update(&doc, &automerge::ROOT));
        assert!(widget.share_pending_path.is_none());
        assert_eq!(
            widget.status.as_deref(),
            Some(t("files-link-copied").as_str())
        );
    }

    fn named_share_status_distinguishes_pending_from_delivered() {
        let pending = t_args(
            named_delivery_status_key(fs_protocol::NamedShareDeliveryStateV1::Pending),
            &[arg("person", "Ada")],
        );
        let delivered = t_args(
            named_delivery_status_key(fs_protocol::NamedShareDeliveryStateV1::Delivered),
            &[arg("person", "Ada")],
        );
        assert!(pending.contains("pending"));
        assert!(delivered.contains("completed"));
        assert_ne!(pending, delivered);
    }

    fn previous_immutable_version_is_an_explicit_restore_command() {
        let previous = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let mut entry = ent("history.txt", false, "text/plain");
        entry.history = fs_protocol::HistoryProjectionV1::parse_xattr(
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
        )
        .ok();
        let mut widget = FileExplorerWidget::new();
        widget.entries = vec![entry];
        widget.selection.select_only(0);
        let expected = fs_protocol::FileVersionId(uuid::Uuid::parse_str(previous).unwrap());
        assert!(
            widget
                .context_menu_spec(&[])
                .iter()
                .any(|(label, command)| {
                    label == "Restore previous version"
                        && *command
                            == Some(FileCommand::RestoreVersion {
                                version_id: expected,
                            })
                })
        );
    }

    #[test]
    fn conflict_resolution_is_explicit_and_merge_preflight_gated() {
        open_text_conflict_menu_requires_review_before_an_exact_merge();
        merge_review_only_enables_apply_after_a_safe_daemon_preflight();
        late_merge_preflight_cannot_repurpose_an_unrelated_confirmation_sheet();
    }

    fn open_text_conflict_menu_requires_review_before_an_exact_merge() {
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let other = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let conflict = uuid::Uuid::parse_str("c0d4e23f-c7cb-4ded-a427-b32cf9da9343").unwrap();
        let mut entry = ent("conflicted.qdoc", false, "application/x-qdoc");
        entry.content_hash = Some([5; 32]);
        entry.history = fs_protocol::HistoryProjectionV1::parse_xattr(
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
        )
        .ok();
        let mut widget = FileExplorerWidget::new();
        widget.entries = vec![entry];
        widget.selection.select_only(0);
        let menu = widget.context_menu_spec(&[]);
        let current_id = fs_protocol::FileVersionId(uuid::Uuid::parse_str(current).unwrap());
        let other_id = fs_protocol::FileVersionId(uuid::Uuid::parse_str(other).unwrap());
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: fs_protocol::ConflictStrategy::KeepCurrent,
                    version_id: current_id,
                })
        }));
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: fs_protocol::ConflictStrategy::UseOther,
                    version_id: other_id,
                })
        }));
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: fs_protocol::ConflictStrategy::KeepBoth,
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

    fn merge_review_only_enables_apply_after_a_safe_daemon_preflight() {
        let conflict_id = uuid::Uuid::from_u128(42);
        let version_id = fs_protocol::FileVersionId(uuid::Uuid::from_u128(43));
        let mut widget = FileExplorerWidget::new();
        widget.show_collab_panel(CollabPanel {
            title: "Review merge".into(),
            summary: "Checking".into(),
            lines: Vec::new(),
            action: None,
        });
        *widget.merge_preflight_result.lock_recover() = Some((
            conflict_id,
            version_id,
            Ok(fs_protocol::MergePreflightProjectionV1 {
                schema_version: fs_protocol::MERGE_PREFLIGHT_VERSION,
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

        *widget.merge_preflight_result.lock_recover() = Some((
            conflict_id,
            version_id,
            Ok(fs_protocol::MergePreflightProjectionV1 {
                schema_version: fs_protocol::MERGE_PREFLIGHT_VERSION,
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

    fn late_merge_preflight_cannot_repurpose_an_unrelated_confirmation_sheet() {
        let stale_conflict = uuid::Uuid::from_u128(50);
        let current_conflict = uuid::Uuid::from_u128(51);
        let version_id = fs_protocol::FileVersionId(uuid::Uuid::from_u128(52));
        let mut widget = FileExplorerWidget::new();
        widget.show_collab_panel(CollabPanel {
            title: "Delete permanently?".into(),
            summary: "This cannot be undone".into(),
            lines: Vec::new(),
            action: Some(CollabPanelAction::EmptyTrash),
        });
        widget.merge_preflight_pending = Some((current_conflict, version_id));
        *widget.merge_preflight_result.lock_recover() = Some((
            stale_conflict,
            version_id,
            Ok(fs_protocol::MergePreflightProjectionV1 {
                schema_version: fs_protocol::MERGE_PREFLIGHT_VERSION,
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

    #[cfg(unix)]
    fn owner_context_menu_revokes_each_exact_active_grant() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("shared.txt");
        std::fs::write(&file, b"shared").unwrap();
        let socket = root.path().join("share-control.sock");
        xattr::set(
            root.path(),
            fs_protocol::SHARE_CONTROL_XATTR,
            socket.as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        let grant = uuid::Uuid::parse_str("c0d4e23f-c7cb-4ded-a427-b32cf9da9343").unwrap();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = file;
        entry.share = fs_protocol::ShareProjectionV1::parse_xattr(
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
        )
        .ok();
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
        assert!(!menu_labels.contains(&"New folder"));
        assert!(!menu_labels.contains(&"Open terminal here"));
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
    use taffy::prelude::*;
    use ui::framework::HostDelivery as _;
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

        /// Lay out a pre-built widget (e.g. one with a home and its standard
        /// favourites, which `new`'s home-less widget lacks).
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
            self.w.sync(&self.state.doc, &self.state.root);
            let node = self.w.build_layout(&mut taffy, &theme, &mut nm);
            let frame = taffy
                .new_with_children(
                    Style {
                        size: Size::from_lengths(width, height),
                        ..Default::default()
                    },
                    &[node],
                )
                .unwrap();
            let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
            // SAFETY: this fixture exclusively owns the exact widget/layout
            // tree and keeps both stable through admission and measurement.
            unsafe {
                bindings.rebuild(&self.w, &taffy, frame, &nm);
                ui::widget::compute_widget_layout(
                    &mut taffy,
                    frame,
                    Size {
                        width: AvailableSpace::Definite(width),
                        height: AvailableSpace::Definite(height),
                    },
                    &nm,
                    &bindings,
                    &mut self.tr,
                )
                .unwrap();
            }
            self.w.apply_layout(&taffy, node, Point::ORIGIN);
            self.taffy = taffy;
            self.node = node;
        }

        fn dispatch(&mut self, ev: &winit::event::WindowEvent) {
            ui::testing::Dispatch::new()
                .doc(&mut self.state.doc)
                .path(&self.state.root)
                .layout(&self.taffy, self.node)
                .send(&mut self.w, ev);
        }

        fn cursor(&mut self, x: f64, y: f64) {
            self.dispatch(&ui::synthetic_input::cursor_moved(x, y));
        }

        fn right_press(&mut self) {
            self.dispatch(&ui::synthetic_input::mouse_input(
                winit::event::ElementState::Pressed,
                winit::event::MouseButton::Right,
            ));
        }

        fn left_release(&mut self) {
            self.dispatch(&ui::synthetic_input::mouse_release());
        }

        fn left_press(&mut self) {
            self.dispatch(&ui::synthetic_input::mouse_press());
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
            let mut scene = vello::Scene::new();
            self.w.draw(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut self.tr,
                &theme,
                &presence,
            );
        }

        fn press_named(&mut self, key: winit::keyboard::NamedKey) {
            let ev = ui::synthetic_input::key_event(
                winit::keyboard::Key::Named(key),
                None,
                winit::event::ElementState::Pressed,
            );
            self.dispatch(&ui::synthetic_input::keyboard_input(ev));
        }

        fn type_char(&mut self, s: &str) {
            let ev = ui::synthetic_input::key_event(
                winit::keyboard::Key::Character(winit::keyboard::SmolStr::new(s)),
                Some(winit::keyboard::SmolStr::new(s)),
                winit::event::ElementState::Pressed,
            );
            self.dispatch(&ui::synthetic_input::keyboard_input(ev));
        }

        /// Press `s` with Ctrl held; whether Files took the key.
        fn ctrl_char(&mut self, s: &str) -> bool {
            let ev = ui::synthetic_input::key_event(
                winit::keyboard::Key::Character(winit::keyboard::SmolStr::new(s)),
                Some(winit::keyboard::SmolStr::new(s)),
                winit::event::ElementState::Pressed,
            );
            ui::testing::Dispatch::new()
                .doc(&mut self.state.doc)
                .path(&self.state.root)
                .layout(&self.taffy, self.node)
                .modifiers(winit::keyboard::ModifiersState::CONTROL)
                .send(&mut self.w, &ui::synthetic_input::keyboard_input(ev))
                .handled
        }

        /// Paint the window's top layer: the notice, the status bubble and
        /// open menus.
        fn draw_overlay(&mut self) {
            let theme = Theme::dark();
            let presence = ui::presence::PresenceCtx::empty();
            let mut scene = vello::Scene::new();
            self.w.draw_overlay(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut self.tr,
                &theme,
                &presence,
            );
        }
    }

    /// Feedback is quiet unless there is something to do about it: selecting
    /// never pops the status bubble, a move to Trash is one notice whose Undo
    /// asks the host for the window's Undo, and Ctrl+Z is the host's.
    #[test]
    fn changes_announce_once_with_the_windows_undo() {
        use crate::file_ops::Change;
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
        h.draw_overlay();
        h.w.status = None;
        h.w.select(Some(1));
        h.draw_overlay();
        assert!(
            !h.w.status_bubble.is_visible(ui::time::now()) || ui::time::wall_frozen(),
            "selecting a file is not news"
        );

        assert!(
            !h.ctrl_char("z"),
            "Ctrl+Z falls through to the window's Undo"
        );

        // The host publishes what the window can undo every frame.
        ui::undo::UndoControl::set_undo_status(&mut h.w, &mut || ui::UndoStatus {
            can_undo: true,
            ..ui::UndoStatus::default()
        });
        let gone = dir.path().join("a.txt").display().to_string();
        h.w.settle_op(
            OpFollowUp::Trash { count: 1 },
            &ui::tilo_resource::Settled::Done(vec![Change::Trashed {
                from: gone,
                entry: Some("/trash/files/a.txt".into()),
            }]),
        );
        let told = h.w.notice_line().expect("a move to Trash is said");
        assert_eq!(
            told,
            t_args("files-moved-to-trash", &[arg("count", 1)]),
            "one plain line, no meta text"
        );
        h.lay_out(1000.0, 720.0);
        h.draw_overlay();
        let undo = h.w.notice.action_rect();
        assert!(undo.width() > 0.0, "the notice paints its Undo");
        h.cursor(undo.center().x, undo.center().y);
        h.left_press();
        h.left_release();
        assert_eq!(
            ui::undo::UndoControl::take_undo_request(&mut h.w),
            Some(ui::UndoRequest::Undo),
            "Undo asks the host, the same door as Ctrl+Z"
        );
        assert!(
            h.w.notice_line().is_none(),
            "acting on the notice takes it down"
        );

        // A rename or a new folder shows in the listing and says nothing.
        h.w.settle_op(
            OpFollowUp::Rename,
            &ui::tilo_resource::Settled::Done(vec![Change::Moved {
                from: dir.path().join("b.txt").display().to_string(),
                to: dir.path().join("d.txt").display().to_string(),
            }]),
        );
        assert!(h.w.notice_line().is_none());
        assert!(h.w.status_line().is_none());

        // A failure says what and why, in words.
        h.w.settle_op(
            OpFollowUp::Rename,
            &ui::tilo_resource::Settled::Failed {
                reason: std::io::Error::from_raw_os_error(13).to_string(),
                partial: vec![],
            },
        );
        let told = h.w.notice_line().expect("a failed rename is said");
        assert!(told.contains(&t("files-error-permission")), "{told}");
        assert!(!told.contains("os error"), "{told}");
    }

    /// Alt+Left, Alt+Right and Alt+Up go back, forward and up; typing the
    /// start of a name moves to it.
    #[test]
    fn keys_go_back_forward_up_and_type_to_a_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        for name in ["beta.txt", "bravo.txt", "charlie.txt"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
        let selected = |h: &H| {
            h.w.selection
                .primary()
                .and_then(|i| h.w.displayed_entries().get(i).map(|e| e.name.clone()))
        };
        h.type_char("b");
        assert_eq!(selected(&h).as_deref(), Some("beta.txt"));
        h.type_char("r");
        assert_eq!(selected(&h).as_deref(), Some("bravo.txt"));

        h.w.navigate(dir.path().join("alpha"));
        h.lay_out(1000.0, 720.0);
        let alt = |h: &mut H, key| {
            let ev = ui::synthetic_input::key_event(
                winit::keyboard::Key::Named(key),
                None,
                winit::event::ElementState::Pressed,
            );
            ui::testing::Dispatch::new()
                .doc(&mut h.state.doc)
                .path(&h.state.root)
                .layout(&h.taffy, h.node)
                .modifiers(winit::keyboard::ModifiersState::ALT)
                .send(&mut h.w, &ui::synthetic_input::keyboard_input(ev))
                .handled
        };
        assert!(alt(&mut h, winit::keyboard::NamedKey::ArrowLeft));
        assert_eq!(h.w.current_path(), dir.path());
        assert!(alt(&mut h, winit::keyboard::NamedKey::ArrowRight));
        assert_eq!(h.w.current_path(), dir.path().join("alpha"));
        assert!(alt(&mut h, winit::keyboard::NamedKey::ArrowUp));
        assert_eq!(h.w.current_path(), dir.path());
    }

    /// A drag resting on a folder opens it, so a drop reaches deeper; a
    /// drag that moved on, or ended, opens nothing.
    #[test]
    fn a_drag_resting_on_a_folder_opens_it() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::with_persistence(None, None);
        w.navigate(dir.path().to_path_buf());
        let now = ui::time::now();
        w.spring = Some((inner.clone(), now));
        assert!(!w.spring_open_if_due(now), "no drag, nothing opens");
        w.pin_drag = Some(dir.path().join("a.txt"));
        assert!(!w.spring_open_if_due(now), "not rested long enough yet");
        assert!(w.spring_open_if_due(now + SPRING_OPEN));
        assert_eq!(w.current_path(), inner);
        assert!(
            w.pin_drag.is_some(),
            "the drag goes on in the opened folder"
        );
    }

    #[test]
    fn rename_selects_the_name_but_not_its_extension() {
        assert_eq!(rename_stem_end("report.final.pdf", false), Some(12));
        assert_eq!(rename_stem_end("notes", false), None);
        assert_eq!(rename_stem_end(".bashrc", false), None);
        assert_eq!(rename_stem_end("photos.2024", true), None);
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

    /// Wide/narrow body modes and the narrow places drawer, each phase on its
    /// own temp dir and harness.
    #[test]
    fn places_list_docks_wide_and_takes_turns_with_the_files_when_narrow() {
        body_docks_wide_and_reflows_to_push_when_narrowed();
        wide_docks_the_places_list_beside_the_files();
        narrow_shows_the_files_not_the_places_list();
        narrow_places_are_a_screen_of_their_own();
        narrow_places_screen_takes_input_the_files_do_not();
        narrow_navigation_returns_to_the_files();
        floating_status_bubble_reserves_a_noninteractive_bottom_lane();
    }

    fn body_docks_wide_and_reflows_to_push_when_narrowed() {
        let mut h = H::new(None, 1200.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            SidebarPlacement::Docked,
            "a wide window docks the places list beside the file view"
        );
        // Resize DOWN on the same instance (a real window resize re-runs
        // apply_layout) — the body must reflow to a single pane, not freeze.
        h.lay_out(480.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            SidebarPlacement::Screen,
            "a narrow window shows one pane at a time"
        );
        // ...and back up again proves it isn't one-way.
        h.lay_out(1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Docked);
    }

    /// Each phase lays out its own harness (or App) at its own size.
    #[test]
    fn toolbar_adapts_to_width_and_its_menus_open_through_every_door() {
        toolbar_reprioritizes_before_desktop_controls_can_overlap_on_phone_width();
        toolbar_more_menu_tracks_its_trigger_and_flips_inside_the_viewport();
        shared_toolbar_resource_action_opens_view_options();
        accesskit_view_options_activation_updates_the_exported_menu_tree();
        archive_extraction_stays_directly_reachable_on_phone_and_desktop();
    }

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

        // Beside the docked places list the folder still gets a 560 px
        // column, which fits the direct controls; only a phone width moves
        // them into the overflow.
        h.lay_out(900.0, 760.0);
        h.lay_out(900.0, 760.0);
        assert!(
            !h.w.toolbar.is_compact(),
            "a 560 px content column still fits the direct controls; only a phone width moves them into the overflow"
        );
        h.lay_out(390.0, 760.0);
        h.lay_out(390.0, 760.0);
        assert!(h.w.toolbar.is_compact(), "back to phone width");

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

    /// Each phase drives real events into its own temp dir and harness.
    #[test]
    fn keyboard_and_pointer_events_drive_rename_menus_and_retry() {
        keyboard_context_menu_reaches_selection_actions_and_executes_them();
        rename_is_edited_in_place_over_the_row();
        typing_in_the_in_place_editor_renames_on_enter();
        narrow_error_retry_uses_the_shared_button_and_recovers();
    }

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
        let intent = ui::widget::controls::take_control_invocation(&mut h.w)
            .expect("the shared button emits a resource action");
        assert_eq!(intent.address.resource, "files.retry");
        assert_eq!(intent.address.action, "invoke");
        assert!(
            crate::files_app::published::invoke(
                &mut h.w,
                &mut h.state.doc,
                &intent.address.resource,
                &intent.address.action,
                &serde_json::Value::Object(intent.arguments),
            )
            .unwrap()
            .is_some(),
            "the retry row refreshes the folder"
        );
        assert!(
            h.w.error().is_none(),
            "the shared button dispatches the normal refresh command"
        );
    }

    fn shared_toolbar_resource_action_opens_view_options() {
        let mut h = H::new(None, 1200.0, 720.0);
        assert!(
            h.w.start_encoded(
                tilo_actions::OperationKind::Effect,
                "tilo.ui.control.invoke",
                br#""files.view_menu""#,
                &mut h.state.doc,
                &h.state.root
            )
            .unwrap()
            .is_some(),
            "the same resource action an AT-SPI client discovers is handled"
        );
        // The button queued its command; the frame after applies it.
        h.lay_out(1200.0, 720.0);
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

    fn accesskit_view_options_activation_updates_the_exported_menu_tree() {
        // This is the same retained-control path used by the Unix AT-SPI
        // provider: find the visual button in an AccessKit update, dispatch
        // its action through App, then require the newly opened menu item in
        // the next update. A `DoAction` reply alone is not enough.
        //
        // The item is matched by role as well as name: the folder's
        // "New folder" resource action is exported as a button whether or not
        // the menu is open, so a name alone cannot tell the opened menu apart
        // from the closed one. The installed-image AT-SPI smoke looks for this
        // same menu item.
        let new_folder_item = t("files-menu-new-folder");
        let exports_new_folder_item = |update: &ui::accesskit::TreeUpdate| {
            update.nodes.iter().any(|(_, node)| {
                node.role() == ui::accesskit::Role::MenuItem
                    && node.label() == Some(new_folder_item.as_str())
                    && !node.is_hidden()
            })
        };
        let mut app = ui::App::new(
            ui::State::new(),
            Box::new(FileExplorerWidget::with_persistence(None, None)),
        );
        app.init_layout();
        let before = app.build_a11y_tree_update(1.0);
        assert!(
            !exports_new_folder_item(&before),
            "the closed View options menu exports no New Folder item"
        );
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
            exports_new_folder_item(&after),
            "AT-SPI activation must publish the actual View options menu, not only acknowledge Click"
        );
    }

    fn archive_extraction_stays_directly_reachable_on_phone_and_desktop() {
        fn names(widget: &dyn Widget, name: &str) -> bool {
            widget
                .accessibility()
                .is_some_and(|node| node.name.as_deref() == Some(name))
                || widget
                    .children()
                    .into_iter()
                    .any(|child| names(child, name))
        }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("note.txt");
        std::fs::write(&source, b"hello").unwrap();
        let archive = temp.path().join("notes.zip");
        crate::archive::create_zip(&[source], &archive).unwrap();

        let mut h = H::new(Some(&archive), 390.0, 760.0);
        let extract = t("files-archive-extract");
        let has_extract = |w: &FileExplorerWidget| names(&w.nav, extract.as_str());
        assert!(has_extract(&h.w), "phone toolbar exposes Extract");
        assert!(
            h.w.toolbar.view_menu_rect().is_none(),
            "archive toolbar must not expose a one-item duplicate menu"
        );
        assert_eq!(h.w.footer_label(), t("files-archive-read-only"));

        h.lay_out(1200.0, 760.0);
        if h.w.take_structure_dirty() {
            h.lay_out(1200.0, 760.0);
        }
        assert!(has_extract(&h.w), "desktop toolbar exposes Extract");
        assert!(h.w.toolbar.view_menu_rect().is_none());
    }

    /// The regression: at ~600 px the window showed the breadcrumb and the
    /// FAVOURITES / SYSTEM list and **no files at all**, while the footer
    /// underneath read "27 items". The places list had been declared a content
    /// overview, so the narrow layout promoted it to the full-screen page and
    /// pushed the file view off the window.
    fn narrow_shows_the_files_not_the_places_list() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
        }
        let h = H::new(Some(dir.path()), 480.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Screen);

        assert!(
            !h.w.debug_places_open(),
            "a narrow window opens on the folder, so the files are what you see",
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
            files.x0 >= 0.0 && files.x1 <= 480.0,
            "and inside the window rather than pushed off it: {files:?}",
        );
        assert!(
            (h.w.debug_content_rect().width() - 480.0).abs() <= f64::EPSILON,
            "the content pane spans the whole narrow window",
        );
    }

    fn floating_status_bubble_reserves_a_noninteractive_bottom_lane() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..40 {
            std::fs::write(dir.path().join(format!("item-{index}.txt")), b"x").unwrap();
        }
        let mut h = H::new(Some(dir.path()), 390.0, 720.0);
        // The grid scrolls under the shared scrollable; the list is a table
        // that scrolls itself and keeps the lane clear in its own layout (see
        // `FileView`'s list tests).
        h.w.set_view_mode(crate::model::ViewMode::Grid);
        h.lay_out(390.0, 720.0);
        let (scrollable, content_viewport) = first_scrollable_viewport(&mut h.w)
            .expect("Files grid is wrapped in the shared scrollable");

        assert!(
            scrollable.y1 - content_viewport.y1
                >= f64::from(status_bubble_safe_inset(&ui::Theme::dark())) - f64::EPSILON,
            "the item/status pill has a dedicated bottom lane rather than overlaying files: \
             scrollable={scrollable:?} viewport={content_viewport:?}"
        );
    }

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
        // back onto the exact FileCommand. Down moves through the rows to
        // Rename, and Enter executes it without any pointer event.
        let rows: Vec<&String> = labels.iter().collect();
        let steps = rows
            .iter()
            .position(|label| label.as_str() == "Rename")
            .expect("Rename is offered")
            + 1;
        for _ in 0..steps {
            h.press_named(winit::keyboard::NamedKey::ArrowDown);
        }
        h.press_named(winit::keyboard::NamedKey::Enter);
        assert!(!h.w.context_menu.is_open());
        assert!(h.w.renaming.is_some(), "keyboard menu executes Rename");
    }

    /// Narrow, the places are a screen of their own: the whole window, the
    /// files one Back away, nothing darkened (HIG §1a).
    fn narrow_places_are_a_screen_of_their_own() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 480.0, 720.0);
        h.w.debug_show_places();
        h.lay_out(480.0, 720.0);
        assert!(h.w.debug_places_open(), "the places screen is in front");
        assert_eq!(h.w.debug_sidebar_rect().width(), 480.0);
        assert!(
            h.rect_of("FileView").is_none(),
            "one column at a time: the files are not under the places"
        );
        assert!(h.w.nav.back(), "Back returns to the files");
        h.w.nav.snap();
        h.lay_out(480.0, 720.0);
        assert!(!h.w.debug_places_open());
        assert!(h.rect_of("FileView").is_some());
    }

    fn narrow_places_screen_takes_input_the_files_do_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("behind-the-places.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 480.0, 720.0);
        h.w.debug_show_places();
        h.lay_out(480.0, 720.0);
        let places = h.w.debug_sidebar_rect();
        h.cursor(places.center().x, places.y0 + 300.0);
        h.right_press();
        assert!(
            !h.w.context_menu.is_open() && h.w.selected_indices().is_empty(),
            "a press on the places screen cannot select or open a menu for a file"
        );
        let semantic_children = h.w.nav.children();
        assert!(
            semantic_children
                .iter()
                .any(|child| child.name() == "NavList"),
            "the places screen exposes the places list"
        );
    }

    /// Picking a destination on the narrow places screen moves on to the
    /// files there.
    fn narrow_navigation_returns_to_the_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut h = H::new(Some(dir.path()), 480.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Screen);
        h.w.debug_show_places();
        h.lay_out(480.0, 720.0);
        assert!(h.w.debug_places_open());

        // A place-navigation (what tapping a place in the list emits). Inject
        // the command the places list pushes, then pump an event so
        // `handle_event` drains it and moves on.
        crate::command::push(&h.w.cmds, FileCommand::Navigate(dir.path().join("sub")));
        h.cursor(10.0, 10.0);
        assert_eq!(
            h.w.current_path(),
            dir.path().join("sub"),
            "the navigation applied"
        );
        assert!(
            !h.w.debug_places_open(),
            "navigating leaves the places for the files",
        );
        h.w.nav.snap();
        h.lay_out(480.0, 720.0);
        assert!(
            (h.w.debug_content_rect().width() - 480.0).abs() <= f64::EPSILON,
            "and the file view owns the whole window",
        );
    }

    /// An inline rename is edited *in place* — a field placed over the row's
    /// name inside the file view — not in a bottom bar. The file view publishes
    /// the field rect during paint; it must sit within the file rows.
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

        // The seed selected the name but not its extension, so typing
        // replaces the name and keeps the type.
        h.type_char("b");
        h.press_named(winit::keyboard::NamedKey::Enter);

        assert!(!h.w.is_renaming(), "Enter commits and closes the editor");
        assert!(
            dir.path().join("b.txt").exists(),
            "the typed name renamed the file on disk"
        );
        assert!(!dir.path().join("a.txt").exists(), "the old name is gone");
    }

    /// Each phase builds its own harness and config dir.
    #[test]
    fn row_drags_and_sidebar_favourites_respond_to_real_pointer_events() {
        dropping_a_folder_on_the_sidebar_pins_it();
        releasing_a_pin_drag_off_the_sidebar_pins_nothing();
        a_row_press_still_becomes_a_drag_after_the_view_is_rebuilt();
    }

    /// Dragging a folder out of the list and dropping it on the docked sidebar
    /// pins it to Favorites; a release off the sidebar pins nothing.
    fn dropping_a_folder_on_the_sidebar_pins_it() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Docked);
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

    fn releasing_a_pin_drag_off_the_sidebar_pins_nothing() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path()),
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
    /// Favorites), not the file-view context menu. The pinned Home entry offers
    /// no Remove. Drives the real right-press path end to end.
    #[test]
    fn right_clicking_a_favourite_opens_its_menu() {
        fn row(w: &dyn Widget, label: &str) -> Option<vello::kurbo::Point> {
            fn node(n: &ui::a11y::AccessNode, label: &str) -> Option<vello::kurbo::Point> {
                if n.name.as_deref() == Some(label) && n.role == ui::a11y::Role::TreeItem {
                    return n.bounds.map(|bounds| {
                        vello::kurbo::Point::new(
                            f64::midpoint(bounds.x0, bounds.x1),
                            f64::midpoint(bounds.y0, bounds.y1),
                        )
                    });
                }
                n.children.iter().find_map(|child| node(child, label))
            }
            w.accessibility()
                .as_ref()
                .and_then(|n| node(n, label))
                .or_else(|| w.children().iter().find_map(|child| row(*child, label)))
        }
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            SidebarPlacement::Docked,
            "wide docks the sidebar"
        );

        let favorites = h.w.favorites_store().borrow().favorites.clone();
        let removable = row(&h.w, &favorites[1].label).expect("the removable favorite is laid out");
        let home_row = row(&h.w, &favorites[0].label).expect("the Home favorite is laid out");
        let (x, y) = (removable.x, removable.y);
        h.cursor(x, y);
        h.right_press();
        let labels = h.open_menu_labels();
        assert!(
            labels
                .iter()
                .any(|l| l == &ui::i18n!("files-menu-remove-favourite")),
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
        let (hx, hy) = (home_row.x, home_row.y);
        h.cursor(hx, hy);
        h.right_press();
        let home = h.open_menu_labels();
        assert!(
            !home.iter().any(|l| l == "Remove from Favorites"),
            "Home is pinned and cannot be removed: {home:?}",
        );
    }

    fn wide_docks_the_places_list_beside_the_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Docked);
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

    /// With the places list not beside the files (hidden), the shell's toggle
    /// sits at the leading edge of the content header; the toolbar starts
    /// after it instead of drawing over it.
    #[test]
    fn the_toolbar_starts_after_the_shells_toggle_when_the_places_list_is_away() {
        let mut h = H::new(None, 1200.0, 720.0);
        h.w.nav.close_sidebar();
        for _ in 0..2 {
            // The inset is discovered by a layout and applied by the rebuild
            // it asks for, as the live frame loop does.
            h.w.nav.snap();
            h.w.take_structure_dirty();
            h.lay_out(1200.0, 720.0);
        }
        let toggle = h.w.nav.geometry().toggle;
        assert!(toggle.width() > 0.0, "the shell shows its toggle");
        let up =
            h.w.toolbar
                .up_button_rect()
                .expect("the toolbar is laid out");
        assert!(
            up.x0 > toggle.x1,
            "the toolbar's controls clear the toggle: toggle={toggle:?} up={up:?}"
        );

        // Beside the files the toggle belongs to the sidebar's own header, so
        // the toolbar needs no room for it.
        h.w.nav.open_sidebar();
        h.w.nav.snap();
        h.lay_out(1200.0, 720.0);
        h.w.take_structure_dirty();
        h.lay_out(1200.0, 720.0);
        let docked = h.w.nav.geometry();
        assert!(docked.toggle.x1 <= docked.content_column.x0 + 1.0);
    }

    /// The state a menu row shows as a check follows the sort in force and
    /// whether hidden files are shown; no other row is a state.
    #[test]
    fn menu_rows_show_the_sort_and_hidden_state_as_checks() {
        let mut w = FileExplorerWidget::with_persistence(None, None);
        assert_eq!(
            w.command_checked(Some(&FileCommand::SetSort(SortBy::Name))),
            Some(true)
        );
        assert_eq!(
            w.command_checked(Some(&FileCommand::SetSort(SortBy::Size))),
            Some(false)
        );
        assert_eq!(
            w.command_checked(Some(&FileCommand::ToggleHidden)),
            Some(false)
        );
        w.show_hidden = true;
        assert_eq!(
            w.command_checked(Some(&FileCommand::ToggleHidden)),
            Some(true)
        );
        assert_eq!(w.command_checked(Some(&FileCommand::NewFolder)), None);
        assert_eq!(w.command_checked(None), None);
    }

    /// A folder that did not open, or is still opening, has no item count to
    /// float over the surface that says what is happening.
    #[test]
    fn the_item_count_is_withheld_while_a_folder_is_loading_or_failed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::with_persistence(None, None);
        w.navigate(dir.path().to_path_buf());
        assert!(!w.footer_label().is_empty());
        w.debug_show_error_state();
        assert!(w.footer_label().is_empty(), "no count over an error");
        w.debug_show_loading_state();
        assert!(w.footer_label().is_empty(), "no count while opening");
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

    /// Each phase starts from its own empty widget.
    #[test]
    fn files_states_and_modal_sheets_publish_accessible_semantics() {
        custom_empty_loading_and_error_states_are_live_semantics();
        deterministic_shot_seams_cover_loading_and_actionable_error_states();
        collaboration_sheet_is_a_focused_modal_semantic_subtree();
        #[cfg(unix)]
        destructive_eviction_requires_a_named_modal_action_before_writing_the_override();
    }

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
        let state = error.children.first().expect("error state semantic group");
        let name = widget
            .current_path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_eq!(
            state.name.as_deref(),
            Some(t_args("files-load-error-title", &[arg("name", name)]).as_str())
        );
        assert_eq!(state.live, Some(Politeness::Assertive));
        let retry = state.children.first().expect("retry semantic action");
        assert_eq!(retry.role, Role::Button);
        assert_eq!(retry.action_target.as_deref(), Some("files-retry-folder"));
    }

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

    fn collaboration_sheet_is_a_focused_modal_semantic_subtree() {
        let (_home, mut widget) = empty_widget();
        widget.show_collab_panel(CollabPanel {
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

    #[cfg(unix)]
    fn destructive_eviction_requires_a_named_modal_action_before_writing_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("only-copy.txt");
        std::fs::write(&file, b"only local copy").unwrap();
        let (_home, mut widget) = empty_widget();
        widget.show_collab_panel(CollabPanel {
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
            xattr::get(&file, fs_protocol::AVAILABILITY_CONTROL_XATTR).unwrap(),
            None,
            "opening the warning must not perform the destructive operation"
        );

        let command = widget
            .collab_panel_action_command()
            .expect("the explicit action is keyboard/click routable");
        widget.close_collab_panel();
        assert!(widget.apply_command(command));
        assert_eq!(
            xattr::get(&file, fs_protocol::AVAILABILITY_CONTROL_XATTR)
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
        // to win over whatever `tilo_paths::home()` says, and it does not say
        // this temp dir.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_USER_HOME", &granted_path);

        let resolved = crate::model::user_home();
        let widget_home = FileExplorerWidget::new()
            .home_path()
            .map(std::path::Path::to_path_buf);

        drop(env);

        assert_eq!(
            resolved.as_deref(),
            Some(granted_path.as_path()),
            "the granted home wins over the process's own HOME",
        );
        assert_ne!(
            tilo_paths::home().as_deref(),
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

// Exercise metadata projection inside the same checked callback entry the
// host uses for its catalogue. Keeping the real component alive preserves
// its registry owner, record lifetime and immutable local-person identity.
pub(super) fn with_owner_callback(callback: impl FnOnce() + 'static) {
    use std::cell::RefCell;
    use ui::widget::Widget;

    struct Probe<F>(RefCell<Option<F>>);
    impl<F: FnOnce() + 'static> ui::resources::PublishesResources for Probe<F> {
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<ui::tilo_actions::Resource> {
            assert!(ui::bound_dispatch::host_person_invocation_permitted());
            self.0.borrow_mut().take().unwrap()();
            Vec::new()
        }
    }
    impl<F: FnOnce() + 'static> Widget for Probe<F> {
        ui::provides!(dyn ui::resources::PublishesResources);
        fn name(&self) -> &'static str {
            "Owner metadata callback"
        }
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            _: &ui::Theme,
            _: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            taffy.new_leaf(taffy::Style::default()).unwrap()
        }
    }
    struct Boundary<F> {
        binding: ui::bound_dispatch::CallbackBinding,
        probe: Probe<F>,
    }
    impl<F: FnOnce() + 'static> Widget for Boundary<F> {
        fn name(&self) -> &'static str {
            "Checked component callback"
        }
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            theme: &ui::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            self.probe.build_layout(taffy, theme, nodes)
        }
        fn callback_binding(&self) -> Option<ui::bound_dispatch::CallbackBinding> {
            Some(self.binding.clone())
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.probe]
        }
    }
    let mut app = ui::prelude::TestApp::<crate::resource::FileExplorerShell>::new().unwrap();
    let boundary = Boundary {
        binding: app.harness().root_widget().callback_binding().unwrap(),
        probe: Probe(RefCell::new(Some(callback))),
    };
    ui::widget::published_resources(&boundary, &automerge::AutoCommit::new());
    assert!(boundary.probe.0.borrow().is_none(), "checked callback ran");
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
            document_opener: DocumentOpenRoute::Unavailable,
            open_with: None,
            local_clipboard: false,
        }
    }

    fn deferred_services() -> HostedFileServices {
        HostedFileServices::deferred_document_open()
    }

    fn owned_child(parent: &Path) -> PathBuf {
        let child = parent.join("files");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("Welcome.txt"), b"Shell capture fixture\n").unwrap();
        child
    }

    #[test]
    fn a_remote_account_is_a_folder_with_a_health_badge() {
        with_owner_callback(remote_account_folder_with_health_badge);
    }

    fn remote_account_folder_with_health_badge() {
        let home = tempfile::tempdir().unwrap();
        let child = owned_child(home.path());
        let mount = home.path().join("Remote/Cloud");
        std::fs::create_dir_all(&mount).unwrap();
        let mut inputs = startup(&child);
        inputs
            .remote_accounts
            .push(crate::remote_accounts::RemoteAccount {
                id: accounts::AccountId::from("cloud"),
                kind: crate::remote_accounts::RemoteAccountKind::Nextcloud,
                label: "Cloud".into(),
                host: "cloud.example".into(),
                health: crate::remote_accounts::RemoteAccountHealth::Offline,
                files_mode: accounts::cloud_types::FilesMode::Sync,
                mount: Some(mount.clone()),
            });
        let mut widget =
            FileExplorerWidget::with_startup_inputs_and_services(inputs, absent_services());
        let mut document = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(&widget, &document);
        let account = resources
            .iter()
            .flat_map(|r| r.children.iter())
            .find(|r| r.kind == "files.remote-account")
            .expect("the account is a resource");
        let presentation = account.presentation.as_ref().expect("presentation");
        assert_eq!(presentation.badge.as_deref(), Some("Offline"));
        let open = account
            .actions
            .iter()
            .find(|a| a.id == crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION)
            .expect("an open action");
        assert_eq!(open.contract.availability, Availability::Available);
        assert!(matches!(
            crate::files_app::published::invoke(
                &mut widget,
                &mut document,
                account.id.as_str(),
                crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION,
                &serde_json::json!({}),
            )
            .unwrap(),
            Some(ui::framework::Effected::Done(_))
        ));
        assert_eq!(
            widget.current_path, mount,
            "opening navigates into the folder"
        );
    }

    /// Each phase owns its fixture directory and fixture-IO guards, dropped before
    /// the next phase begins.
    #[test]
    fn injected_startup_uses_only_explicit_host_inputs_and_services() {
        injected_startup_and_reload_keep_the_host_directory_and_catalogs();
        injected_services_absence_never_builds_thumbnails_or_uses_global_opener();
        injected_listing_and_refresh_use_only_explicit_workspace_roots();
        fixture_io_guards_stop_before_following_io();
    }

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
            files_mode: accounts::cloud_types::FilesMode::Sync,
            mount: None,
        };
        let mut inputs = startup(&child);
        inputs.remote_accounts.push(account.clone());
        let mut widget =
            FileExplorerWidget::with_startup_inputs_and_services(inputs, absent_services());
        assert_eq!(widget.current_path, child);
        assert_eq!(widget.displayed_entries().len(), 1);
        assert_eq!(widget.displayed_entries()[0].name, "Welcome.txt");
        ui::widget::Widget::set_focus(&mut widget, true);
        let document = automerge::AutoCommit::new();
        ui::widget::Widget::sync(&mut widget, &document, &automerge::ROOT);
        widget.refresh_external_search_catalogs();
        assert!(!widget.refresh_trusted_identities(None));
        widget.refresh_metadata_if_due(ui::time::now() + metadata_refresh::LIVE_INTERVAL);
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
        let told = widget.notice_line().expect("a failed open is said");
        assert!(told.contains(&t("files-open-unavailable")), "{told}");
        assert_eq!(calls.get(), 0);
        assert_eq!(guard.attempts(), 0);
        assert_eq!(listings.observations(), vec![(child, false)]);
    }

    #[test]
    fn deferred_document_open_reports_the_shell_refusal_on_the_next_sync() {
        let parent = tempfile::tempdir().unwrap();
        let child = owned_child(parent.path());
        let mut widget = FileExplorerWidget::with_startup_inputs_and_services(
            startup(&child),
            deferred_services(),
        );
        // Nearby Send asks nothing until the shell announces a change, so it
        // sets no deadline; nothing may be due yet.
        assert!(
            Widget::next_wake_deadline(&widget).is_none_or(|deadline| deadline > ui::time::now()),
            "the completed-reply branch, rather than Nearby Send, must supply the immediate wake"
        );
        widget
            .request_deferred_document_open(
                child.join("Welcome.txt"),
                ui::widget::DocumentOpenCapability::issue(),
            )
            .unwrap();
        let Some(ui::widget::AppHostIntent::OpenDocument { token, reply, .. }) =
            widget.take_host_intent()
        else {
            panic!("deferred route must hand the path to its shell host");
        };
        let reply = reply.upgrade().expect("widget retains its pending reply");
        reply
            .borrow_mut()
            .complete(token.wrapping_add(1), Err("stale reply".into()));
        assert!(
            reply.borrow().is_pending(token),
            "a stale shell completion must not replace this request"
        );
        reply
            .borrow_mut()
            .complete(token, Err("document storage unavailable".into()));
        assert!(
            Widget::next_wake_deadline(&widget).is_some_and(|deadline| deadline <= ui::time::now()),
            "a shell reply after draw must schedule the one follow-up Files sync"
        );
        let document = automerge::AutoCommit::new();
        Widget::sync(&mut widget, &document, &automerge::ROOT);
        let told = widget.notice_line().expect("the shell's refusal is said");
        assert!(
            told.starts_with(&t_args("files-open-failed", &[arg("name", "Welcome.txt")])),
            "{told}"
        );
        assert!(told.contains("document storage unavailable"), "{told}");
        assert!(widget.pending_document_open.is_none());
    }

    #[test]
    fn dropped_deferred_document_request_cannot_be_admitted() {
        let parent = tempfile::tempdir().unwrap();
        let child = owned_child(parent.path());
        let reply = {
            let mut widget = FileExplorerWidget::with_startup_inputs_and_services(
                startup(&child),
                deferred_services(),
            );
            widget
                .request_deferred_document_open(
                    child.join("Welcome.txt"),
                    ui::widget::DocumentOpenCapability::issue(),
                )
                .unwrap();
            let Some(ui::widget::AppHostIntent::OpenDocument { reply, .. }) =
                widget.take_host_intent()
            else {
                panic!("deferred route must have an owner-bound reply");
            };
            reply
        };
        assert!(reply.upgrade().is_none());
    }

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

mod sort_selection_tests {
    use super::*;

    #[test]
    fn sorting_keeps_an_active_rename_on_the_original_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), b"aaa").unwrap();
        std::fs::write(root.path().join("b.txt"), b"b").unwrap();
        let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
        files.navigate(root.path().into());
        files.set_sort_by(SortBy::Name);
        files.apply_command(FileCommand::SetSortOrder(SortOrder::Ascending));
        files.apply_command(FileCommand::Click {
            index: 0,
            ctrl: false,
            shift: false,
        });
        files.apply_command(FileCommand::Rename);
        files.set_rename_buffer("chosen.txt".into());
        let mut doc = automerge::AutoCommit::new();
        let folder = published::tree(&files, &doc)[0].id.clone();
        published::invoke(
            &mut files,
            &mut doc,
            folder.as_str(),
            "sort-size",
            &serde_json::json!({}),
        )
        .unwrap();
        assert!(files.is_renaming());
        assert!(files.rename_input.is_focused());
        assert_eq!(files.rename_buffer, "chosen.txt");
        assert!(files.commit_rename());
        assert_eq!(
            std::fs::read(root.path().join("chosen.txt")).unwrap(),
            b"aaa"
        );
        assert_eq!(std::fs::read(root.path().join("b.txt")).unwrap(), b"b");
        assert!(!root.path().join("a.txt").exists());
    }

    #[test]
    fn sort_capture_stages_keep_the_same_selected_files() {
        use crate::resource::FileExplorerShell;
        use ui::prelude::TestApp;
        for scenario in ["selection-sort", "selection-sorted", "selection-reversed"] {
            let mut app = TestApp::<FileExplorerShell>::capture(Some(scenario)).unwrap();
            app.with_surface(|files: &mut FileExplorerWidget| {
                let mut names: Vec<_> = files
                    .selected_paths()
                    .iter()
                    .map(|path| path.file_name().unwrap().to_owned())
                    .collect();
                names.sort();
                assert_eq!(names, vec!["Notes.txt", "Recipes.md"]);
                assert_eq!(files.view_mode(), ViewMode::List);
                assert_eq!(
                    files.sort_by(),
                    if scenario == "selection-sort" {
                        SortBy::Name
                    } else {
                        SortBy::Size
                    }
                );
                assert!(files.status.is_none());
            })
            .unwrap();
        }
    }

    #[test]
    fn every_published_sort_keeps_selection_and_an_empty_selection_stays_empty() {
        let root = tempfile::tempdir().unwrap();
        for (name, bytes) in [("a.txt", "aaa"), ("b.md", "b"), ("c.txt", "cc")] {
            std::fs::write(root.path().join(name), bytes).unwrap();
        }
        let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
        files.navigate(root.path().into());
        let selected_path = root.path().join("a.txt");
        let index = files
            .entries
            .iter()
            .position(|entry| entry.path == selected_path)
            .unwrap();
        files.apply_command(FileCommand::Click {
            index,
            ctrl: true,
            shift: false,
        });
        let mut doc = automerge::AutoCommit::new();
        let folder = published::tree(&files, &doc)[0].id.clone();
        for action in [
            "sort-size",
            "sort-type",
            "sort-modified",
            "sort-descending",
            "sort-name",
            "sort-ascending",
        ] {
            published::invoke(
                &mut files,
                &mut doc,
                folder.as_str(),
                action,
                &serde_json::json!({}),
            )
            .unwrap();
            assert_eq!(
                files.selected_paths(),
                vec![selected_path.clone()],
                "{action}"
            );
            assert_eq!(
                files.entries[files.selection.primary().unwrap()].path,
                selected_path
            );
            assert_eq!(
                files.entries[files.selection.anchor().unwrap()].path,
                selected_path
            );
        }
        files.selection.clear();
        files.toggle_sort_order();
        assert!(files.selection.is_empty());
        assert_eq!(files.selection.primary(), None);
        assert_eq!(files.selection.anchor(), None);
    }

    #[test]
    fn sorting_keeps_the_range_anchor_and_a_deselected_keyboard_cursor() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            std::fs::write(root.path().join(name), name).unwrap();
        }
        let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
        files.navigate(root.path().into());
        files.set_sort_by(SortBy::Name);
        files.apply_command(FileCommand::SetSortOrder(SortOrder::Ascending));
        files.apply_command(FileCommand::Click {
            index: 0,
            ctrl: true,
            shift: false,
        });
        files.apply_command(FileCommand::Click {
            index: 2,
            ctrl: false,
            shift: true,
        });
        files.toggle_sort_order();
        assert_eq!(files.selection.anchor(), Some(3));
        assert_eq!(files.selection.primary(), Some(1));
        files.apply_command(FileCommand::Click {
            index: 2,
            ctrl: false,
            shift: true,
        });
        assert_eq!(files.selection.selected(), vec![2, 3]);
        files.apply_command(FileCommand::Click {
            index: 2,
            ctrl: true,
            shift: false,
        });
        assert!(!files.selection.is_selected(2));
        files.toggle_sort_order();
        assert_eq!(files.selection.selected(), vec![0]);
        assert_eq!(files.selection.anchor(), Some(1));
        assert_eq!(files.selection.primary(), Some(1));
        files.apply_command(FileCommand::Click {
            index: 3,
            ctrl: false,
            shift: true,
        });
        assert_eq!(files.selection.selected(), vec![1, 2, 3]);
    }

    #[test]
    fn sorting_does_not_open_another_file_from_a_previous_click() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
        files.navigate(root.path().into());
        files.set_sort_by(SortBy::Name);
        files.apply_command(FileCommand::SetSortOrder(SortOrder::Ascending));
        files.apply_command(FileCommand::Click {
            index: 0,
            ctrl: false,
            shift: false,
        });
        files.toggle_sort_order();
        files.apply_command(FileCommand::Click {
            index: 0,
            ctrl: false,
            shift: false,
        });
        assert_eq!(files.current_path, root.path());
        assert_eq!(files.selected_paths(), vec![root.path().join("b")]);
    }

    #[test]
    fn sorting_preserves_the_files_copied_by_a_multiselection() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        for (name, bytes) in [("a.txt", "aaa"), ("b.txt", "b"), ("c.txt", "cc")] {
            std::fs::write(source.join(name), bytes).unwrap();
        }
        let mut files =
            FileExplorerWidget::with_persistence(Some(root.path()), Some(root.path().into()));
        files.navigate(source.clone());
        files.set_sort_by(SortBy::Name);
        files.apply_command(FileCommand::SetSortOrder(SortOrder::Ascending));
        for index in [0, 2] {
            files.apply_command(FileCommand::Click {
                index,
                ctrl: true,
                shift: false,
            });
        }
        let mut doc = automerge::AutoCommit::new();
        let folder = published::tree(&files, &doc)[0].id.clone();
        published::invoke(
            &mut files,
            &mut doc,
            folder.as_str(),
            "sort-size",
            &serde_json::json!({}),
        )
        .unwrap();
        let mut paths = files.selected_paths();
        paths.sort();
        assert_eq!(paths, vec![source.join("a.txt"), source.join("c.txt")]);
        assert!(files.apply_command(FileCommand::Copy));
        files.navigate(destination.clone());
        assert!(files.apply_command(FileCommand::Paste));
        assert_eq!(std::fs::read(destination.join("a.txt")).unwrap(), b"aaa");
        assert_eq!(std::fs::read(destination.join("c.txt")).unwrap(), b"cc");
        assert!(!destination.join("b.txt").exists());
        assert_eq!(std::fs::read(source.join("a.txt")).unwrap(), b"aaa");
    }
}

mod recent_resource_tests {
    use super::*;
    use crate::effects::FilesEffect;
    use crate::resource::FileExplorerShell;
    use ui::prelude::TestApp;

    #[test]
    fn recent_capture_stages_actual_rows_and_effect_outcomes() {
        for scenario in [
            "recent",
            "recent-menu",
            "recent-revealed",
            "recent-forgotten",
        ] {
            let mut app = TestApp::<FileExplorerShell>::capture(Some(scenario)).unwrap();
            app.with_surface(|files: &mut FileExplorerWidget| {
                assert!(
                    files.status.is_none(),
                    "a capture must not hide a failed effect"
                );
                match scenario {
                    "recent-revealed" => {
                        assert!(!files.is_recent_view());
                        let selected = files.selected_index().unwrap();
                        assert_eq!(files.entries()[selected].name, "Quarterly report.pdf");
                        assert!(files.entries()[selected].path.exists());
                    }
                    "recent-forgotten" => {
                        assert!(files.is_recent_view());
                        assert_eq!(files.entries().len(), 1);
                        assert_eq!(files.entries()[0].name, "Notes.txt");
                        assert!(
                            files.entries()[0]
                                .path
                                .parent()
                                .unwrap()
                                .join("Quarterly report.pdf")
                                .exists()
                        );
                    }
                    _ => {
                        assert!(files.is_recent_view());
                        assert_eq!(files.entries().len(), 2);
                        if scenario == "recent-menu" {
                            assert!(
                                files
                                    .context_menu
                                    .items()
                                    .iter()
                                    .any(|item| item.label() == Some("Open containing folder"))
                            );
                        }
                    }
                }
            })
            .unwrap();
        }
    }

    fn invoke(app: &mut TestApp<FileExplorerShell>, row: &str, action: &str) {
        let instance = app.harness().component_instance_id().unwrap();
        app.harness()
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: instance,
                    address: ui::tilo_actions::ActionAddress::new(row, action),
                },
                &serde_json::Map::new(),
            )
            .unwrap();
        app.settle();
    }
    #[test]
    fn recent_resource_reveals_and_forgets_without_changing_files() {
        let root = tempfile::tempdir().unwrap();
        let reports = root.path().join("Reports");
        std::fs::create_dir(&reports).unwrap();
        let target = reports.join("budget.txt");
        let sibling = reports.join("notes.txt");
        std::fs::write(&target, b"budget").unwrap();
        std::fs::write(&sibling, b"notes").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[root.path().to_str().unwrap()]).unwrap();
        let shared_before = app.state();
        app.with_surface(|files: &mut FileExplorerWidget| {
            files.note_recent(&sibling, crate::recent::RecentUse::Opened);
            files.note_recent(&target, crate::recent::RecentUse::Opened);
        })
        .unwrap();
        app.effect(&FilesEffect::Recent).unwrap();
        let row = crate::search_provider::entry_resource_id(&target);
        let published = app
            .with_surface(|files: &mut FileExplorerWidget| files.published_row(&row).unwrap())
            .unwrap();
        for expected in ["reveal", "forget-recent"] {
            assert!(
                published.actions.iter().any(|action| action.id == expected),
                "Recent's {expected} must be available through the resource door"
            );
            assert_eq!(
                published
                    .actions
                    .iter()
                    .find(|action| action.id == expected)
                    .unwrap()
                    .kind,
                ui::tilo_actions::OperationKind::View,
                "Recent navigation and device history do not mutate shared files"
            );
        }
        app.leave_room();
        for action in ["reveal", "forget-recent"] {
            assert!(app.call_on(&row, action, serde_json::json!({})).is_err());
        }
        app.with_surface(|files: &mut FileExplorerWidget| {
            assert!(files.is_recent_view());
            assert_eq!(files.entries().len(), 2);
        })
        .unwrap();
        app.set_role(ui::permissions::Role::Viewer);
        invoke(&mut app, &row, "reveal");
        app.with_surface(|files: &mut FileExplorerWidget| {
            assert_eq!(files.current_path(), reports);
            let selected = files.selected_index().unwrap();
            assert_eq!(files.entries()[selected].path, target);
        })
        .unwrap();
        app.effect(&FilesEffect::Back).unwrap();
        invoke(&mut app, &row, "forget-recent");
        app.with_surface(|files: &mut FileExplorerWidget| {
            assert!(files.published_row(&row).is_none());
            assert_eq!(files.entries().len(), 1);
            assert_eq!(files.entries()[0].path, sibling);
        })
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"budget");
        assert_eq!(std::fs::read(&sibling).unwrap(), b"notes");
        assert_eq!(
            app.state(),
            shared_before,
            "shared bookmarks and file operations stay unchanged"
        );
        assert_eq!(
            app.settings::<crate::recent::RecentFiles>()
                .get()
                .items()
                .len(),
            1
        );
        assert!(
            app.call_on(&row, "forget-recent", serde_json::json!({}))
                .is_err()
        );
        let mut reopened = app.reopen().unwrap();
        reopened.effect(&FilesEffect::Recent).unwrap();
        reopened
            .with_surface(|files: &mut FileExplorerWidget| {
                assert_eq!(files.entries().len(), 1);
                assert_eq!(files.entries()[0].path, sibling);
            })
            .unwrap();
    }

    #[test]
    fn retained_recent_actions_refuse_other_views_and_deleted_rows() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("report.txt");
        std::fs::write(&target, b"report").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[root.path().to_str().unwrap()]).unwrap();
        app.with_surface(|files: &mut FileExplorerWidget| {
            files.note_recent(&target, crate::recent::RecentUse::Opened);
        })
        .unwrap();
        let row = crate::search_provider::entry_resource_id(&target);
        for effect in [
            FilesEffect::RevealRecent { entry: row.clone() },
            FilesEffect::ForgetRecent { entry: row.clone() },
        ] {
            assert!(
                app.effect(&effect).is_err(),
                "a folder row is not a Recent target"
            );
        }
        assert_eq!(
            app.settings::<crate::recent::RecentFiles>()
                .get()
                .items()
                .len(),
            1
        );
        app.effect(&FilesEffect::Recent).unwrap();
        for action in ["reveal", "forget-recent"] {
            assert!(
                app.call_on(
                    &row,
                    action,
                    serde_json::json!({"entry": root.path().display().to_string()})
                )
                .is_err(),
                "bound targets cannot be replaced by caller paths"
            );
        }
        std::fs::remove_file(&target).unwrap();
        for action in ["reveal", "forget-recent"] {
            assert!(
                app.call_on(&row, action, serde_json::json!({})).is_err(),
                "an unrefreshed but deleted row is stale"
            );
        }
        app.with_surface(|files: &mut FileExplorerWidget| {
            assert!(files.is_recent_view());
            files.refresh();
            assert!(files.published_row(&row).is_none());
        })
        .unwrap();
    }
}

#[test]
fn completed_clipboard_undo_feedback_survives_the_actual_listing_refresh() {
    let home = tempfile::tempdir().unwrap();
    let mut widget = FileExplorerWidget::with_persistence(None, Some(home.path().to_owned()));
    widget.navigate(home.path().to_owned());
    widget.follow_ops(&ui::tilo_resource::Outbox::new());
    let operation = "e44ac38c-627f-48a9-ad92-4a6cdf249216".to_owned();
    let mut ops = ui::tilo_resource::Outbox::new();
    let id = ops.push(
        ui::tilo_resource::Id::from_raw("clipboard-undo"),
        crate::resource::FileTask {
            device: "local".into(),
            op: crate::file_ops::FileOp::Revert {
                op: operation.clone(),
            },
        },
    );
    ops.settle(
        &id,
        ui::tilo_resource::Settled::Done(vec![crate::file_ops::Change::ClipboardUndo {
            operation,
        }]),
    );
    widget.follow_ops(&ops);
    assert_eq!(
        widget.status.as_deref(),
        Some(t("files-clipboard-undone").as_str())
    );
    assert!(widget.entries.is_empty());
}

mod oauth_cloud_location_tests {
    use super::*;
    use crate::remote_accounts::{self, RemoteAccountHealth};

    const PROVIDERS: [(&str, &str); 4] = [
        ("a-google", remote_accounts::GOOGLE_DRIVE_TYPE),
        ("b-onedrive", remote_accounts::ONEDRIVE_TYPE),
        ("c-dropbox", remote_accounts::DROPBOX_TYPE),
        ("d-icloud", remote_accounts::ICLOUD_DRIVE_TYPE),
    ];

    fn fixture_store(root: &Path) -> accounts::AccountStore {
        let mut store = accounts::AccountStore::new();
        for (id, provider) in PROVIDERS {
            let mut account = accounts::Account::new(id, provider);
            account.display_name = "Cloud".into();
            // Projection must select only public provider facts, not forward
            // arbitrary stored configuration or imply authenticated access.
            account.config = serde_json::json!({"unsupported_private_field":"secret-sentinel"});
            store.upsert(account);
        }
        store.save_in(root).unwrap();
        accounts::AccountSyncReporter::in_root(accounts::AccountId::from("a-google"), root)
            .offline()
            .unwrap();
        store
    }

    #[test]
    fn canonical_oauth_accounts_publish_sidebar_resources_and_open_exact_cloud_mounts() {
        with_owner_callback(canonical_oauth_accounts);
    }

    fn canonical_oauth_accounts() {
        let config = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _store = fixture_store(config.path());
        let mut widget = FileExplorerWidget::with_persistence_dirs(
            None,
            Some(home.path().to_owned()),
            favorites::UserDirs::default(),
        );
        widget.accounts_root = Some(config.path().to_owned());
        widget.refresh_external_search_catalogs();
        let mut document = automerge::AutoCommit::new();
        let accounts = widget.remote_account_snapshot();
        assert_eq!(accounts.len(), PROVIDERS.len());
        let first = accounts
            .iter()
            .find(|account| account.id.as_str() == "a-google")
            .unwrap();
        assert_eq!(first.health, RemoteAccountHealth::Offline);
        assert!(
            accounts
                .iter()
                .filter(|account| account.id.as_str() != "a-google")
                .all(|account| account.health == RemoteAccountHealth::NeverChecked)
        );
        for (index, (id, provider)) in PROVIDERS.iter().enumerate() {
            let account = accounts
                .iter()
                .find(|account| account.id.as_str() == *id)
                .unwrap();
            assert_eq!(remote_accounts::type_id(account.kind).as_str(), *provider);
            let name = if index == 0 {
                "Cloud".into()
            } else {
                format!("Cloud ({})", index.checked_add(1).unwrap())
            };
            let mount = home.path().join("Remote").join(name);
            assert_eq!(account.mount.as_ref(), Some(&mount));
            std::fs::create_dir_all(&mount).unwrap();
            std::fs::write(mount.join("Selected-provider.txt"), id.as_bytes()).unwrap();
            let places = widget.current_places().sections();
            assert!(places.iter().flat_map(|section| &section.items).any(|row| {
                row.id == format!("loc:{}", mount.display()) && row.label == "Cloud"
            }));
            let resource_id = remote_accounts::resource_id(&account.id);
            let resources = crate::files_app::published::tree(&widget, &document);
            let resource = resources
                .iter()
                .flat_map(|root| &root.children)
                .find(|resource| resource.id.as_str() == resource_id)
                .unwrap();
            assert_eq!(resource.kind, "files.remote-account");
            let open = resource
                .actions
                .iter()
                .find(|action| action.id == crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION)
                .unwrap();
            assert_eq!(open.contract.availability, Availability::Available);
            assert!(
                !serde_json::to_string(&resource.presentation)
                    .unwrap()
                    .contains("secret-sentinel")
            );
            assert!(matches!(
                crate::files_app::published::invoke(
                    &mut widget,
                    &mut document,
                    &resource_id,
                    crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION,
                    &serde_json::json!({})
                )
                .unwrap(),
                Some(ui::framework::Effected::Done(_))
            ));
            assert_eq!(widget.current_path, mount);
            assert_eq!(
                std::fs::read(widget.current_path.join("Selected-provider.txt")).unwrap(),
                id.as_bytes()
            );
        }
        assert!(
            !accounts::vault_dir_in(config.path()).exists(),
            "projection and open must not create credential custody"
        );
    }

    #[test]
    fn disabled_and_removed_oauth_accounts_update_existing_places_and_semantic_actions() {
        with_owner_callback(disabled_and_removed_oauth_accounts);
    }

    fn disabled_and_removed_oauth_accounts() {
        let config = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut store = fixture_store(config.path());
        let mut disabled = store
            .get(&accounts::AccountId::from("c-dropbox"))
            .unwrap()
            .clone();
        disabled.enabled = false;
        store.upsert(disabled);
        store.save_in(config.path()).unwrap();
        let mut widget = FileExplorerWidget::with_persistence_dirs(
            None,
            Some(home.path().to_owned()),
            favorites::UserDirs::default(),
        );
        widget.accounts_root = Some(config.path().to_owned());
        widget.refresh_external_search_catalogs();
        let mut document = automerge::AutoCommit::new();
        let places = widget.current_places().sections();
        assert!(places.iter().flat_map(|section| &section.items).any(|row| {
            row.id == "account:c-dropbox" && row.secondary.as_deref() == Some("Disabled")
        }));
        let id = remote_accounts::resource_id(&accounts::AccountId::from("c-dropbox"));
        let resources = crate::files_app::published::tree(&widget, &document);
        let resource = resources
            .iter()
            .flat_map(|root| &root.children)
            .find(|resource| resource.id.as_str() == id)
            .unwrap();
        assert!(resource.actions.iter().any(|action| {
            action.id == crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION
                && matches!(&action.contract.availability, Availability::Unavailable { code, .. } if code == "account_disabled")
        }));
        store.remove(&accounts::AccountId::from("c-dropbox"));
        store.save_in(config.path()).unwrap();
        widget.refresh_external_search_catalogs();
        assert!(
            !widget
                .current_places()
                .sections()
                .iter()
                .flat_map(|section| &section.items)
                .any(|row| row.id == "account:c-dropbox")
        );
        let resources = crate::files_app::published::tree(&widget, &document);
        assert!(
            !resources
                .iter()
                .flat_map(|root| &root.children)
                .any(|resource| resource.id.as_str() == id)
        );
        assert!(
            crate::files_app::published::invoke(
                &mut widget,
                &mut document,
                &id,
                crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION,
                &serde_json::json!({})
            )
            .is_err()
        );
    }
}

/// Selected files are shared with an agent as their rows' published
/// resources, each a whole item, in the order shown.
#[test]
fn selected_files_are_shared_as_the_items_an_agent_acts_on() {
    let folder = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(folder.path().join(name), name).unwrap();
    }
    let mut files = FileExplorerWidget::new();
    files.navigate(folder.path().to_path_buf());
    assert!(ui::shared_selection::selection_of(&files).is_none());
    let displayed = files.displayed_entries();
    let index = |name: &str| {
        displayed
            .iter()
            .position(|entry| entry.name == name)
            .unwrap()
    };
    files.selection.select_only(index("a.txt"));
    files.selection.toggle(index("c.txt"));
    let selection = ui::shared_selection::selection_of(&files).expect("two files selected");
    let ids: Vec<String> = ["a.txt", "c.txt"]
        .iter()
        .map(|name| FileExplorerWidget::entry_resource_id(&displayed[index(name)]))
        .collect();
    assert_eq!(
        selection
            .anchor
            .items
            .iter()
            .map(|item| item.item.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert!(
        selection
            .anchor
            .items
            .iter()
            .all(|item| item.start.is_none() && item.at.is_none())
    );
    assert_eq!(
        selection.anchor.excerpt.as_deref(),
        Some("- a.txt\n- c.txt")
    );
    assert_eq!(
        selection.resource_kind.as_deref(),
        Some(<crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND)
    );
}

/// Moving into a folder with a taken name asks once; each answer does what
/// it says, and Replace sends the existing item to Trash rather than
/// destroying it.
#[test]
fn a_name_clash_asks_once_and_each_answer_does_what_it_says() {
    let _home = tilo_test_env::TestHome::new().expect("private test home");
    let setup = || {
        let dir = tempfile::tempdir().unwrap();
        let (src, dest) = (dir.path().join("src"), dir.path().join("dest"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(src.join("a.txt"), b"new").unwrap();
        std::fs::write(src.join("b.txt"), b"new").unwrap();
        std::fs::write(dest.join("a.txt"), b"old").unwrap();
        let mut w = FileExplorerWidget::with_persistence(None, None);
        w.navigate(src.clone());
        w.drop_paths_into(&[src.join("a.txt"), src.join("b.txt")], &dest, false);
        assert!(w.clash.is_some(), "a taken name asks first");
        assert!(
            src.join("a.txt").exists(),
            "nothing moved before the answer"
        );
        (dir, w, src, dest)
    };
    let names = |dest: &std::path::Path| {
        let mut names: Vec<String> = std::fs::read_dir(dest)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    let (_dir, mut w, src, dest) = setup();
    w.settle_clash(Some(ClashChoice::Skip));
    assert_eq!(names(&dest), ["a.txt", "b.txt"]);
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");
    assert!(src.join("a.txt").exists(), "the skipped item stays");

    let (_dir, mut w, _src, dest) = setup();
    w.settle_clash(Some(ClashChoice::KeepBoth));
    assert_eq!(names(&dest).len(), 3, "{:?}", names(&dest));

    let (_dir, mut w, _src, dest) = setup();
    w.settle_clash(Some(ClashChoice::Replace));
    assert_eq!(names(&dest), ["a.txt", "b.txt"]);
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"new");

    let (_dir, mut w, src, _dest) = setup();
    w.settle_clash(None);
    assert!(
        w.clash.is_none() && src.join("a.txt").exists(),
        "Escape changes nothing"
    );
}

/// A tracked copy counts its bytes, and Cancel stops it with nothing past
/// the item it was on.
#[test]
fn a_tracked_copy_reports_bytes_and_stops_on_cancel() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().unwrap();
    let (src, dest) = (dir.path().join("big.bin"), dir.path().join("dest"));
    std::fs::write(&src, vec![7_u8; 3 << 20]).unwrap();
    std::fs::create_dir(&dest).unwrap();
    let op = crate::file_ops::FileOp::Copy {
        paths: vec![src.display().to_string()],
        folder: dest.display().to_string(),
    };
    let seen = std::cell::Cell::new(0_u64);
    let outcome = crate::file_ops::execute_tracked(&op, None, &|transfer| {
        seen.set(transfer.done.load(Ordering::Relaxed));
    });
    assert!(outcome.reason().is_none(), "{:?}", outcome.reason());
    assert_eq!(seen.get(), 3 << 20, "the last report has every byte");
    assert!(crate::file_ops::active_transfers().is_empty());

    let cancelling = |transfer: &crate::file_ops::LocalTransfer| {
        transfer.cancel.store(true, Ordering::Relaxed);
    };
    let large = dir.path().join("large.bin");
    std::fs::write(&large, vec![7_u8; 9 << 20]).unwrap();
    let op = crate::file_ops::FileOp::Copy {
        paths: vec![large.display().to_string(), src.display().to_string()],
        folder: dest.display().to_string(),
    };
    // Cancel at the first report, part way through the large file.
    let outcome = crate::file_ops::execute_tracked(&op, None, &cancelling);
    assert_eq!(outcome.reason(), Some(crate::file_ops::CANCELLED));
    assert!(
        outcome.result().is_empty(),
        "the item it stopped in, and the ones after, are not there"
    );
}

/// Space shows the selected file large and closes it again; arrows move on.
#[test]
fn space_previews_the_selection_and_closes_again() {
    use winit::keyboard::{Key, NamedKey};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"first line\nsecond").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"other").unwrap();
    let mut w = FileExplorerWidget::with_persistence(None, None);
    w.navigate(dir.path().to_path_buf());
    w.select(Some(0));
    let space = ui::synthetic_input::key_event(
        Key::Named(NamedKey::Space),
        None,
        winit::event::ElementState::Pressed,
    );
    assert!(w.handle_app_key(&space, winit::event::Modifiers::default()));
    assert_eq!(w.preview.as_ref().map(Preview::title), Some("a.txt"));
    let down = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(NamedKey::ArrowDown),
        None,
        winit::event::ElementState::Pressed,
    ));
    w.route_preview_event(&down, winit::event::Modifiers::default());
    assert_eq!(w.preview.as_ref().map(Preview::title), Some("b.txt"));
    w.route_preview_event(
        &ui::synthetic_input::keyboard_input(space),
        winit::event::Modifiers::default(),
    );
    assert!(w.preview.is_none(), "Space closes it again");
}

/// Files dropped in from another app are copied into the folder shown.
#[test]
fn files_dropped_from_another_app_are_copied_here() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    for name in ["one.txt", "two.txt"] {
        std::fs::write(src.path().join(name), name.as_bytes()).unwrap();
    }
    let mut w = FileExplorerWidget::with_persistence(None, None);
    w.navigate(dest.path().to_path_buf());
    for name in ["one.txt", "two.txt"] {
        assert!(
            w.route_incoming_drop(&winit::event::WindowEvent::DroppedFile(
                src.path().join(name)
            ))
        );
    }
    w.take_incoming_drop();
    assert_eq!(
        std::fs::read(dest.path().join("one.txt")).unwrap(),
        b"one.txt"
    );
    assert_eq!(
        std::fs::read(dest.path().join("two.txt")).unwrap(),
        b"two.txt"
    );
    assert!(
        src.path().join("one.txt").exists(),
        "a drop from another app copies"
    );
}

/// A file inside an archive opens as a read-only copy, made once.
#[test]
fn a_file_inside_an_archive_opens_as_a_read_only_copy() {
    let _home = tilo_test_env::TestHome::new().expect("private test home");
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("docs");
    std::fs::create_dir(&docs).unwrap();
    std::fs::write(docs.join("readme.txt"), b"read me").unwrap();
    let zip = dir.path().join("bundle.zip");
    crate::archive::create_zip(&[docs], &zip).unwrap();
    let member = crate::archive::list(&zip)
        .unwrap()
        .into_iter()
        .find(|entry| !entry.is_dir)
        .expect("a file in the archive")
        .path;
    let copy = archive_member_copy(&zip, &member).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"read me");
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
    assert_eq!(archive_member_copy(&zip, &member).unwrap(), copy, "reused");
    let missing = archive_member_copy(&zip, std::path::Path::new("nope.txt"));
    assert!(missing.is_err());
}
