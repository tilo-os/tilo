use super::*;
use ui::tilo_resource::Effect as _;
use ui::widget::{Collaborative as _, SidebarPlacement};

/// A stand-in for the session's api: what Files asked for, and what it answers.
pub(super) mod session_api {
    use runtime_portal::tools::DocumentApp;
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex, MutexGuard};

    #[derive(Default)]
    struct Api {
        opens: Vec<(PathBuf, Option<String>)>,
        refusal: Option<String>,
        candidates: Vec<DocumentApp>,
        listing_refusal: Option<String>,
        listings: usize,
    }

    static SHARED_API: LazyLock<Mutex<Api>> = LazyLock::new(|| Mutex::new(Api::default()));

    fn api() -> MutexGuard<'static, Api> {
        SHARED_API
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn start() {
        *api() = Api::default();
    }

    pub(super) fn record_open(path: &Path, app: Option<&str>) -> Result<(), String> {
        let mut api = api();
        api.opens.push((path.to_path_buf(), app.map(str::to_owned)));
        match api.refusal.take() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    pub(super) fn record_candidates(_path: &Path) -> Result<Vec<DocumentApp>, String> {
        let mut api = api();
        api.listings += 1;
        match api.listing_refusal.take() {
            Some(reason) => Err(reason),
            None => Ok(api.candidates.clone()),
        }
    }

    pub(super) fn refuse_listing(reason: &str) {
        api().listing_refusal = Some(reason.into());
    }

    pub(super) fn listing(apps: Vec<DocumentApp>) {
        api().candidates = apps;
    }

    pub(super) fn refuse(reason: &str) {
        api().refusal = Some(reason.into());
    }

    pub(super) fn asks() -> Vec<String> {
        api()
            .opens
            .iter()
            .map(|(path, app)| format!("{} {}", path.display(), app.as_deref().unwrap_or("-")))
            .collect()
    }

    pub(super) fn opened() -> Vec<PathBuf> {
        api().opens.iter().map(|(path, _)| path.clone()).collect()
    }

    pub(super) fn listings() -> usize {
        api().listings
    }
}

pub(super) fn asking_session(w: &FileExplorerWidget) {
    w.state().open_request = session_api::record_open;
    w.state().document_candidates = session_api::record_candidates;
}

pub(super) fn settle_opens(w: &FileExplorerWidget) {
    tilo_test_env::wait_until("the window to take in the open's answer", || {
        w.state().take_open_replies()
    });
}

#[cfg(test)]
mod open_with_tests {
    use super::session_api::{self, asks, listings};
    use super::*;
    use crate::model::Entry;
    use runtime_portal::tools::DocumentApp;
    use std::path::{Path, PathBuf};

    fn pdf_apps() -> Vec<DocumentApp> {
        vec![
            DocumentApp {
                id: "org.gnome.Evince.desktop".into(),
                name: "Evince".into(),
                icon: Some("file".into()),
                source: Some(runtime_portal::tools::AppKind::System),
                artwork: Some(ui::png::encode_rgba8(1, 1, &[200, 30, 30, 255]).expect("a png")),
                is_default: true,
            },
            DocumentApp {
                id: "so.tilo.pdf-viewer".into(),
                name: "PDF Viewer".into(),
                icon: Some("file".into()),
                source: None,
                artwork: None,
                is_default: false,
            },
        ]
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
            document_id: None,
            availability: None,
            app: None,
            rich_metadata: None,
            observed: None,
        }
    }

    tilo_test_env::scenario! {
        fn opening_files_asks_the_session_and_open_with_lists_its_apps;

        phase files_are_classified_for_open_and_merge_by_extension;
        {
        }

        phase activation_hands_every_file_to_the_session_to_open;
        {
            session_api::start();
            let w = FileExplorerWidget::new();
            asking_session(&w);
            w.state().entries = ["report.odt", "photo.png", "slides.pdf", "main.rs"]
                .iter()
                .map(|f| file_entry(f))
                .collect();
            for i in 0..4 {
                assert!(w.state().open_entry(i).unwrap(), "entry {i} opens");
                settle_opens(&w);
            }

            assert_eq!(
                asks(),
                vec![
                    "/docs/report.odt -",
                    "/docs/photo.png -",
                    "/docs/slides.pdf -",
                    "/docs/main.rs -",
                ],
                "every file is handed to the session, with its own path"
            );
            assert_eq!(w.state().status, None, "a successful open reports nothing");
        }

        phase a_refused_open_is_shown_to_the_user;
        {
            session_api::start();
            session_api::refuse("no installed app opens .zzz files (thing.zzz)");
            let w = FileExplorerWidget::new();
            asking_session(&w);
            w.state().entries = vec![file_entry("thing.zzz")];
            assert!(w.state().open_entry(0).unwrap());
            settle_opens(&w);
            let told = w
                .state()
                .notice_line()
                .expect("the refusal is said in a notice");
            assert!(told.contains("thing.zzz"), "must name the file: {told}");
            assert!(told.contains("no installed app"), "and say why: {told}");
            assert_eq!(
                asks(),
                vec!["/docs/thing.zzz -"],
                "a refusal is not retried behind the session's back"
            );
        }

        phase open_with_lists_the_default_first_and_opens_exactly_the_chosen_app;
        {
            let mut w = pdf_widget();
            w.state().rect = vello::kurbo::Rect::new(0.0, 0.0, 1200.0, 800.0);
            w.state().open_context_menu(Point::new(40.0, 40.0));
            assert!(menu_labels(&w.chrome().context_menu).contains(&"Open with".to_string()));

            // Choosing "Open with" keeps its menu open and lists the apps
            // beside its own row.
            let chrome = w.chrome();
            let menu = &mut chrome.context_menu;
            let row = menu
                .items()
                .iter()
                .position(|item| item.label() == Some("Open with"))
                .expect("an Open with row");
            menu.click_item(row);
            assert!(menu.is_open(), "the row menu stays open");
            assert!(menu.is_submenu_open(), "the row opens its apps");
            let row_rect = menu.item_rect(row).expect("the row is shown");
            let nested = *menu
                .painted_bounds(&ui::Theme::dark())
                .last()
                .expect("the apps are painted");
            assert!(
                nested.center().x > menu.get_rect().x1,
                "beside the menu, not over it: {nested:?}"
            );
            assert!(
                nested.y0 < row_rect.y1 && nested.y1 > row_rect.y0,
                "level with its row {row_rect:?}: {nested:?}"
            );
            let apps = menu.items()[row].submenu_items().expect("the apps").to_vec();
            assert_eq!(
                apps.iter()
                    .filter_map(|item| item.label().map(str::to_string))
                    .collect::<Vec<_>>(),
                vec!["Evince (default)", "PDF Viewer", "Other application…"]
            );
            assert!(menu.choose_submenu_item(row, 1));
            let viewer = chrome.take_chosen().expect("an app row");
            assert_eq!(
                viewer,
                FileCommand::OpenWith {
                    path: PathBuf::from("/docs/slides.pdf"),
                    app_id: "so.tilo.pdf-viewer".into(),
                }
            );
            // Each row wears the app's own glyph, and a row two apps share a name with says where the
            // machine got it.
            // An app the session handed artwork for wears it, its glyph beside it.
            let rows: Vec<(Option<ui::icons::Icon>, Option<String>, Option<String>)> = apps
                .iter()
                .map(|item| match item {
                    ui::widget::context_menu::MenuItem::Action {
                        icon,
                        detail,
                        image,
                        ..
                    } => (
                        *icon,
                        detail.clone(),
                        image.as_ref().map(|image| image.key().to_owned()),
                    ),
                    _ => (None, None, None),
                })
                .collect();
            assert_eq!(
                rows[0],
                (
                    Some(ui::icons::Icon::File),
                    None,
                    Some("app:org.gnome.Evince.desktop".into())
                )
            );
            assert_eq!(rows[1], (Some(ui::icons::Icon::File), None, None));
            w.state().apply_command(viewer);
            settle_opens(&w);
            assert_eq!(asks(), vec!["/docs/slides.pdf so.tilo.pdf-viewer"]);
            assert_eq!(w.state().status, None, "a successful open reports nothing");
        }

        phase open_with_is_offered_only_for_one_regular_file;
        {
            let w = pdf_widget();
            w.state().entries.push(Entry {
                is_dir: true,
                ..file_entry("Folder")
            });
            w.state().selection.select_only(2);
            assert_eq!(w.state().open_with_subject(), None, "folders open in Files");
            w.state().selection.select_only(0);
            w.state().selection.toggle(1);
            assert_eq!(w.state().open_with_subject(), None, "one file at a time");
        }

        phase open_with_says_where_an_app_came_from_only_beside_one_of_its_name;
        {
            let w = pdf_widget();
            let details = |w: &FileExplorerWidget| -> Vec<Option<String>> {
                let (rows, _) = w.state().open_with_rows();
                rows.iter()
                    .filter_map(|row| match row {
                        MenuItem::Action { detail, .. } => Some(detail.clone()),
                        _ => None,
                    })
                    .collect()
            };
            assert!(
                details(&w).iter().all(Option::is_none),
                "two apps of two names: {:?}",
                details(&w)
            );
            let mut same = pdf_apps();
            same[1].name = "evince ".into();
            same[1].source = Some(runtime_portal::tools::AppKind::Tilo);
            session_api::listing(same);
            w.state().invalidate_open_with();
            let told = details(&w);
            assert!(
                told.iter().take(2).all(Option::is_some),
                "two apps of one name say where each came from: {told:?}"
            );
            assert_ne!(told[0], told[1], "and the two read apart");
        }

        phase a_type_no_app_opens_says_so_and_opens_nothing;
        {
            let w = pdf_widget();
            session_api::listing(Vec::new());
            w.state().invalidate_open_with();
            let (rows, _) = w.state().open_with_rows();
            assert_eq!(labels(&rows), vec!["No application opens this file"]);
            w.state().open_app_chooser();
            assert_eq!(w.state().status, Some(t("files-open-with-none")));
            assert!(session_api::opened().is_empty(), "nothing was launched");
        }

        phase a_listing_the_session_cannot_answer_offers_no_apps;
        {
            let w = pdf_widget();
            session_api::refuse_listing("the session did not answer");
            w.state().invalidate_open_with();
            let (rows, _) = w.state().open_with_rows();
            assert_eq!(labels(&rows), vec!["No application opens this file"]);
        }

        phase open_with_is_a_typed_action_on_file_resources;
        {
            let mut w = pdf_widget();
            let mut doc = automerge::AutoCommit::new();
            let resources = crate::files_app::published::tree(&w, &doc);
            let entry_id = crate::search_provider::entry_resource_id(Path::new("/docs/slides.pdf"));
            let spec = ui::tilo_actions::api::spec_of(&resources, &entry_id, "open-with")
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
            settle_opens(&w);
            assert_eq!(asks(), vec!["/docs/slides.pdf so.tilo.pdf-viewer"]);
            assert_eq!(
                app.choice_label("org.gnome.Evince.desktop"),
                Some("Evince (default)"),
                "each choice reads as the app's name, the default handler named"
            );
            assert!(
                ui::tilo_actions::api::spec_of(&resources, &entry_id, "open-with:so.tilo.pdf-viewer")
                    .is_none(),
                "the apps that open a file are the one action's choices, not an action each"
            );
        }

        phase other_application_searches_every_app_to_open_once;
        {
            let mut w = pdf_widget();
            w.state().apply_command(FileCommand::OpenWithOther);
            let entry_id = crate::search_provider::entry_resource_id(Path::new("/docs/slides.pdf"));
            let invocation =
                ui::widget::controls::take_control_invocation(&mut w).expect("the host's search opens");
            let request = ui::search::SearchRequest::from_arguments(&invocation.arguments);
            assert_eq!(
                request.part().map(|scope| scope.anchor.as_str()),
                Some(entry_id.as_str()),
                "on the file"
            );
            let mut doc = automerge::AutoCommit::new();
            let resources = crate::files_app::published::tree(&w, &doc);
            let spec = ui::tilo_actions::api::spec_of(&resources, &entry_id, "open-with")
                .expect("the file offers the one open-with, its apps as choices");
            let app = spec
                .params()
                .iter()
                .find(|param| param.name == "app")
                .expect("an app parameter");
            assert_eq!(
                app.choices.iter().map(ToString::to_string).collect::<Vec<_>>(),
                vec!["org.gnome.Evince.desktop", "so.tilo.pdf-viewer"],
                "every app that opens the file is a choice"
            );
            assert_eq!(
                app.schema
                    .as_ref()
                    .and_then(|schema| schema.get("default"))
                    .and_then(serde_json::Value::as_str),
                Some("org.gnome.Evince.desktop"),
                "the default choice is the app that would open it anyway"
            );
            let mut args = serde_json::Map::new();
            args.insert("app".into(), "org.gnome.Evince.desktop".into());
            let encoded = spec.build_json(&args).unwrap();
            crate::files_app::published::start(
                &mut w,
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
                &mut doc,
                &automerge::ROOT,
            )
            .unwrap();
            settle_opens(&w);
            assert_eq!(asks(), vec!["/docs/slides.pdf org.gnome.Evince.desktop"]);
        }

        phase candidates_are_asked_once_per_type_per_listing;
        {
            let w = pdf_widget();
            let doc = automerge::AutoCommit::new();
            let _ = crate::files_app::published::tree(&w, &doc);
            let _ = crate::files_app::published::tree(&w, &doc);
            assert_eq!(
                listings(),
                2,
                "one ask for .pdf and one for .txt, then cached"
            );
        }
    }

    pub(super) fn context_menu_opens_with_selection_aware_items() {
        let mut w = FileExplorerWidget::new();
        w.state().entries = vec![file_entry("a.txt"), file_entry("b.txt")];
        w.state().selection.select_only(0);
        w.state().open_context_menu(Point::new(40.0, 40.0));
        assert!(w.chrome().context_menu.is_open());
        let labels: Vec<String> = w
            .chrome()
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(std::string::ToString::to_string))
            .collect();
        for expected in ["Open", "Rename", "Cut", "Copy", "Move to Trash"] {
            assert!(
                labels.contains(&expected.to_string()),
                "menu missing {expected}: {labels:?}"
            );
        }
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
        assert!(!labels.contains(&"Paste".to_string()));
    }

    pub(super) fn menus_never_separate_a_lone_item() {
        let mut w = FileExplorerWidget::new();
        w.state().entries = vec![file_entry("a.txt"), file_entry("b.txt")];
        for selected in [false, true] {
            w.state().selection.clear();
            if selected {
                w.state().selection.select_only(0);
            }
            w.state().open_context_menu(Point::new(10.0, 10.0));
            let separators: Vec<bool> = w
                .chrome()
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

    pub(super) fn the_folder_menu_holds_the_folder_actions() {
        let mut w = FileExplorerWidget::new();
        w.state().entries = vec![file_entry("a.txt")];
        w.state().open_context_menu(Point::new(10.0, 10.0));
        let labels: Vec<String> = w
            .chrome()
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
        w.state().entries = vec![file_entry("a.txt")];
        w.state().open_context_menu(Point::new(10.0, 10.0));
        let labels: Vec<String> = w
            .chrome()
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
        w.state().entries = vec![file_entry("a.txt")];
        w.state().selection.select_only(0);
        w.state().apply_command(crate::command::FileCommand::Copy);
        assert_eq!(w.state().clipboard.len(), 1);
        w.state().open_context_menu(Point::new(10.0, 10.0));
        assert!(
            w.chrome()
                .context_menu
                .items()
                .iter()
                .filter_map(|i| i.label().map(std::string::ToString::to_string))
                .any(|label| label == "Paste")
        );
    }

    fn menu_labels(menu: &ContextMenu) -> Vec<String> {
        labels(menu.items())
    }

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| item.label().map(str::to_string))
            .collect()
    }

    fn pdf_widget() -> FileExplorerWidget {
        session_api::start();
        session_api::listing(pdf_apps());
        let w = FileExplorerWidget::new();
        asking_session(&w);
        w.state().current_path = PathBuf::from("/docs");
        w.state().entries = vec![file_entry("slides.pdf"), file_entry("notes.txt")];
        w.state().selection.select_only(0);
        w
    }

    fn displayed_names(w: &FileExplorerWidget) -> Vec<String> {
        w.state()
            .displayed_entries()
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
        session_api::start();
        let w = FileExplorerWidget::with_persistence(Some(config.path()), None);
        asking_session(&w);
        w.state().navigate(folder.path().to_path_buf());
        assert_eq!(displayed_names(&w), vec!["a.txt", "b.txt", "c.txt"]);
        for name in ["b.txt", "a.txt", "c.txt", "a.txt"] {
            let index = displayed_names(&w)
                .iter()
                .position(|candidate| candidate == name)
                .unwrap();
            assert!(w.state().open_entry(index).unwrap());
            settle_opens(&w);
        }
        assert_eq!(
            session_api::opened().len(),
            4,
            "the session opened every file"
        );

        w.state()
            .apply_command(FileCommand::Navigate(crate::recent::location()));
        assert!(w.state().error.is_none(), "{:?}", w.state().error);
        assert_eq!(
            displayed_names(&w),
            vec!["a.txt", "c.txt", "b.txt"],
            "recency order, not the folder's name sort"
        );
        assert_eq!(
            crate::breadcrumbs::breadcrumb_components(&w.state().current_path, None, &[])
                .into_iter()
                .map(|(label, _)| label)
                .collect::<Vec<_>>(),
            vec!["Recent"]
        );
    }

    tilo_test_env::scenario! {
        fn recent_survives_restarts_and_renames_and_forgets_on_request;

        phase recent_is_a_device_setting_that_survives_a_restart;
        {
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
                    files
                        .state()
                        .note_recent(&folder.path().join(name), crate::recent::RecentUse::Opened);
                }
            })
            .unwrap();
            let stored = app
                .settings::<crate::resource::DeviceRecentFiles>()
                .get()
                .by_workspace
                .get(ui::framework::testing::TEST_WORKSPACE)
                .cloned()
                .unwrap_or_default();
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
                        .state()
                        .recent_entries()
                        .iter()
                        .map(|entry| entry.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap();
            assert_eq!(names, vec!["a.txt", "b.txt"]);
        }

        phase recent_forgets_deleted_files_and_on_request;
        {
            let config = tempfile::tempdir().unwrap();
            let folder = tempfile::tempdir().unwrap();
            for name in ["keep.txt", "gone.txt", "drop.txt"] {
                std::fs::write(folder.path().join(name), name).unwrap();
            }
            let mut w = FileExplorerWidget::with_persistence(Some(config.path()), None);
            for name in ["keep.txt", "gone.txt", "drop.txt"] {
                w.state()
                    .note_recent(&folder.path().join(name), crate::recent::RecentUse::Changed);
            }
            std::fs::remove_file(folder.path().join("gone.txt")).unwrap();
            w.state().navigate(crate::recent::location());
            assert_eq!(displayed_names(&w), vec!["drop.txt", "keep.txt"]);

            w.state().selection.select_only(0);
            w.state().open_context_menu(Point::new(20.0, 20.0));
            let labels = menu_labels(&w.chrome().context_menu);
            assert!(
                labels.contains(&"Open containing folder".to_string()),
                "{labels:?}"
            );
            assert!(labels.contains(&"Remove from Recent".to_string()));
            assert!(!labels.contains(&"New folder".to_string()), "{labels:?}");
            assert!(!labels.contains(&"Paste".to_string()));

            w.state()
                .apply_command(FileCommand::ForgetRecent(folder.path().join("drop.txt")));
            assert_eq!(displayed_names(&w), vec!["keep.txt"]);
            assert!(folder.path().join("drop.txt").exists(), "the file stays");

            w.state().apply_command(FileCommand::NewFolder);
            assert_eq!(
                w.state().status.as_deref(),
                Some("Recent is not a folder; open one to add files")
            );

            w.state().selection.select_only(0);
            w.state()
                .apply_command(FileCommand::RevealInFolder(folder.path().join("keep.txt")));
            assert_eq!(w.state().current_path, folder.path());
            let selected = w.state().selection.primary().unwrap();
            assert_eq!(w.state().displayed_entries()[selected].name, "keep.txt");
        }

        phase a_rename_keeps_the_file_in_recent;
        {
            let config = tempfile::tempdir().unwrap();
            let folder = tempfile::tempdir().unwrap();
            std::fs::write(folder.path().join("draft.txt"), "x").unwrap();
            let w = FileExplorerWidget::with_persistence(Some(config.path()), None);
            w.state().navigate(folder.path().to_path_buf());
            w.state().note_recent(
                &folder.path().join("draft.txt"),
                crate::recent::RecentUse::Opened,
            );
            w.state().selection.select_only(0);
            w.state().begin_rename();
            w.state().set_rename_buffer("final.txt".into());
            assert!(w.state().commit_rename());
            assert_eq!(
                w.state()
                    .recent_entries()
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect::<Vec<_>>(),
                vec!["final.txt"]
            );
        }
    }
}

#[cfg(test)]
mod command_path_tests {
    use super::*;
    use crate::model::Entry;
    use std::path::PathBuf;
    #[cfg(target_os = "linux")]
    use tilo_test_env::SpawnUnderEnvLock as _;

    tilo_test_env::scenario! {
        fn folder_commands_navigate_organise_and_guard_through_published_actions;

        phase scheduled_refresh_and_record_search_preserve_the_view;
        {
            opaque_hold_deadline_reaches_files_root_before_metadata_refresh();
            #[cfg(unix)]
            scheduled_metadata_refresh_preserves_resource_selection_and_scroll();
            #[cfg(unix)]
            collaborative_search_activation_resolves_the_latest_path_by_document_id();
        }

        phase folder_resource_exports_canonical_actions_handlers_and_ids;
        {
            resource_catalogue_exposes_view_and_favourite_commands_through_the_action_api();
            every_exported_local_file_handler_is_declared_for_the_broker();
            exported_background_operations_have_valid_job_contracts();
            resource_catalogue_is_bounded_but_an_exact_visible_entry_resolves_lazily();
            template_resource_ids_are_stable_bounded_and_do_not_disclose_paths();
            a_mounted_remote_account_opens_even_offline_but_not_when_disabled();
        }

        phase entry_resource_actions_are_opaque_and_fail_closed;
        {
            resource_open_refuses_a_deleted_or_replayed_entry_target();
            local_resource_actions_carry_only_opaque_ids_not_host_paths();
            entry_resource_uses_a_wire_safe_id_that_survives_a_rename();
            projected_availability_controls_are_discoverable_and_use_the_entry_route();
            nearby_receiver_actions_are_opaque_confirmed_and_current();
        }

        phase trash_and_archives_guard_destructive_actions;
        {
            trash_actions_require_an_explicit_irrevocable_confirmation();
            archives_browse_as_read_only_folders_and_extract_from_inside();
        }

        phase context_menus_follow_selection_clipboard_location_and_templates;
        {
            super::open_with_tests::context_menu_opens_with_selection_aware_items();
            super::open_with_tests::empty_selection_menu_omits_item_actions();
            super::open_with_tests::the_folder_menu_holds_the_folder_actions();
            super::open_with_tests::menus_never_separate_a_lone_item();
            super::open_with_tests::copy_then_paste_menu_includes_paste();
            #[cfg(unix)]
            trash_view_menu_offers_restore_and_empty_not_move_to_trash();
            context_menu_exposes_the_sort_section_including_type();
            context_select_targets_the_clicked_row_not_a_stale_selection();
            a_screen_reader_opens_a_rows_menu_on_the_row_it_names();
            context_menu_checks_new_file_on_templates_existing();
            large_text_view_menu_clamps_inside_the_live_window();
        }

        phase navigation_listing_history_and_error_recovery;
        {
            state_surface_copy_names_the_folder_and_offers_what_can_help();
            retry_refresh_recovers_from_error_once_the_dir_exists();
            history_back_and_forward_traverse_visited_dirs();
            double_click_click_command_navigates_into_a_directory();
            a_populated_directory_lists_exactly_its_contents();
            scroll_position_survives_a_structural_rebuild_but_resets_on_navigation();
        }

        phase go_to_move_to_and_copy_to_organise_files_by_absolute_path;
        {
            use ui::tilo_actions::api;
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
                let address = api::published(&resources, &resource, action).unwrap();
                let encoded = api::encode_json(&resources, &address, &arguments)
                    .map_err(|error| error.to_string())?;
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
            assert_eq!(files.state().current_path, downloads);
            let folder =
                <crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND.to_owned();
            let relative = invoke(
                &mut files,
                &mut doc,
                (folder, "go-to"),
                ("path", Path::new("in")),
            );
            let refused = relative.unwrap_err();
            assert!(
                refused.contains("`path`") && refused.contains("does not decode"),
                "a relative path is refused where its argument decodes, before the window sees it: {refused}"
            );

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
                (notes.clone(), "copy-to"),
                ("folder", &nowhere),
            );
            assert!(missing.unwrap_err().contains("is not an existing folder"));
            let relative = invoke(
                &mut files,
                &mut doc,
                (notes, "copy-to"),
                ("folder", Path::new("documents")),
            );
            let refused = relative.unwrap_err();
            assert!(
                refused.contains("`folder`") && refused.contains("does not decode"),
                "a relative folder is refused where its argument decodes: {refused}"
            );
        }

        phase showing_a_folder_answers_with_its_resource_and_entries;
        {
            use ui::tilo_actions::api;
            let root = tempfile::tempdir().unwrap();
            let downloads = root.path().join("in/downloads");
            std::fs::create_dir_all(downloads.join("old")).unwrap();
            for index in 0..70 {
                std::fs::write(downloads.join(format!("f{index:02}.txt")), "x").unwrap();
            }
            let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
            files.state().navigate(root.path().into());
            let mut doc = automerge::AutoCommit::new();
            let mut show = |action: &str, path: &Path| {
                let resources = crate::files_app::published::tree(&files, &doc);
                let folder = <crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND;
                let address = api::published(&resources, folder, action).unwrap();
                let encoded = api::encode_json(
                    &resources,
                    &address,
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

        phase clipboard_copy_cut_paste_moves_files_and_retries_failures;
        {
            copy_then_paste_duplicates_the_file_into_the_current_dir();
            cut_then_paste_moves_the_file_into_the_current_dir();
            partial_cut_paste_retains_only_the_sources_that_need_retrying();
            #[cfg(unix)]
            exported_paste_preserves_dangling_destination_links();
            exported_cut_paste_reports_destination_failure_and_keeps_retryable_source();
        }

        phase favourites_edit_the_sidebar_and_follow_localized_user_dirs;
        {
            favorite_commands_add_reorder_and_remove();
            favourites_follow_localized_xdg_user_dirs();
        }
    }

    tilo_test_env::scenario! {
        #[cfg(unix)]
        fn an_idle_window_waits_for_changes_instead_of_polling;

        phase idle_directory_refresh_does_not_request_frames;
        {
            use std::collections::HashMap;
            use taffy::prelude::*;
            use ui::theme::Theme;
            use vello::kurbo::Point;

            let mut env = tilo_test_env::ScopedEnv::new();
            let _identities = absent_identity_projection(&mut env);
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("a.txt"), b"ok").unwrap();
            let mut widget = FileExplorerWidget::new();
            widget.state().load(dir.path().to_path_buf());
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

            widget.state().next_metadata_refresh = Some(ui::time::now());
            let _ = widget.state().refresh_metadata_if_due(ui::time::now());
            widget.state().next_metadata_refresh = Some(ui::time::now());
            assert!(
                !widget.state().refresh_metadata_if_due(ui::time::now()),
                "an idle Files poll must not keep requesting frames"
            );

            ui::widget::Widget::mark_drawn(&mut widget);
            let _ = widget.take_structure_dirty();
            assert_idle_turn_is_clean(&mut widget, &state, "idle scheduled sync");

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
            assert_watched_idle_window_needs_no_wake(&widget);
        }

        phase a_missing_identity_projection_is_awaited_not_polled;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            let identities = absent_identity_projection(&mut env);
            let dir = tempfile::tempdir().unwrap();
            let widget = FileExplorerWidget::new();
            widget.state().load(dir.path().to_path_buf());
            let now = ui::time::now();
            let _ = widget.state().refresh_metadata_if_due(now);
            assert!(widget.state().trusted_humans.humans().is_empty());
            assert!(!widget.state().identity_watch.watches_target());
            assert_ne!(
                widget.state().identity_watch.coverage(),
                crate::directory_watch::Coverage::None,
                "the projection's creation is watched for"
            );
            #[cfg(target_os = "linux")]
            assert_eq!(
                widget.state().next_metadata_refresh,
                None,
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
            tilo_test_env::wait_until("the new identity projection is read", || {
                let _ = widget.state().refresh_metadata_if_due(now);
                !widget.state().trusted_humans.humans().is_empty()
            });
            assert_eq!(
                widget.state().trusted_humans.humans()[0].display_name,
                "Ada"
            );
            assert!(
                widget.state().identity_watch.watches_target(),
                "the watch moved onto the projection's directory"
            );
        }

        phase idle_files_has_no_periodic_deadline_with_a_subscription;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            let _identities = quiet_refresh_sources(&mut env);
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("a.txt"), b"ok").unwrap();
            let widget = FileExplorerWidget::new();
            widget.state().load(dir.path().to_path_buf());
            let now = ui::time::now();
            let _ = widget.state().refresh_metadata_if_due(now);
            assert!(!widget.state().directory_changed, "nothing changed");
            assert_ne!(
                widget.state().directory_watch.coverage(),
                crate::directory_watch::Coverage::None,
                "the open directory is watched"
            );
            #[cfg(target_os = "linux")]
            assert_eq!(
                widget.state().next_metadata_refresh,
                None,
                "a fully watched directory is never polled"
            );
            if let Some(due) = widget.state().next_metadata_refresh {
                assert!(
                    due >= now + metadata_refresh::LIVE_INTERVAL,
                    "only a partially watched directory keeps its poll"
                );
            }
        }

        phase idle_files_over_a_watched_workspace_root_has_no_catalogue_poll;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            let _identities = quiet_refresh_sources(&mut env);
            let root = tempfile::tempdir().unwrap();
            let folder = root.path().join("folder");
            std::fs::create_dir(&folder).unwrap();
            env.set("TILO_WORKSPACE_ROOTS", root.path());
            let widget = FileExplorerWidget::new();
            assert!(
                widget
                    .state()
                    .collaborative_resource_pager
                    .borrow()
                    .has_roots(),
                "the root holds a resource catalogue"
            );
            widget.state().load(folder);
            let _ = widget.state().refresh_metadata_if_due(ui::time::now());
            assert_ne!(
                widget.state().catalog_watch.coverage(),
                crate::directory_watch::Coverage::None,
                "the catalogue's root is watched"
            );
            assert_watched_idle_window_needs_no_wake(&widget);

            std::fs::write(root.path().join("new.txt"), b"new").unwrap();
            tilo_test_env::wait_until(
                "the root's change to move the catalogue's generation",
                || {
                    #[cfg(target_os = "linux")]
                    assert_eq!(
                        widget.state().next_metadata_refresh,
                        None,
                        "nothing is polled"
                    );
                    widget.state().refresh_metadata_if_due(ui::time::now())
                },
            );
            assert_watched_idle_window_needs_no_wake(&widget);
        }

        phase a_file_created_in_the_open_directory_appears_without_a_poll;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            let _identities = quiet_refresh_sources(&mut env);
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("a.txt"), b"ok").unwrap();
            let widget = FileExplorerWidget::new();
            widget.state().load(dir.path().to_path_buf());
            let _ = widget.state().refresh_metadata_if_due(ui::time::now());
            std::fs::write(dir.path().join("b.txt"), b"new").unwrap();
            let shown = |widget: &FileExplorerWidget| {
                widget
                    .state()
                    .entries
                    .iter()
                    .any(|entry| entry.name == "b.txt")
            };
            tilo_test_env::wait_until("the new file to be listed", || {
                let readable = {
                    let state = widget.state();
                    state.last_directory_read + metadata_refresh::change_read_gap(state.entries.len())
                };
                if let Some(due) = widget.state().next_metadata_refresh {
                    assert!(readable < due, "the change is read before the poll");
                }
                let _ = widget.state().refresh_metadata_if_due(readable);
                shown(&widget)
            });
            assert!(
                !widget.state().directory_changed,
                "the reported change was read"
            );
        }
    }

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

    #[cfg(unix)]
    fn assert_watched_idle_window_needs_no_wake(widget: &FileExplorerWidget) {
        widget.state().next_metadata_refresh = None;
        let _ = widget.state().refresh_metadata_if_due(ui::time::now());
        assert_eq!(
            widget.next_wake_deadline(),
            widget.state().next_metadata_refresh,
            "nothing but a metadata poll may schedule an idle window"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            widget.next_wake_deadline(),
            None,
            "a fully watched idle Files window never wakes the host"
        );
    }

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

    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_put_in_a_folder_appears_in_files_without_reopening_the_folder() {
        const SIZE: u64 = 5 << 30;
        let mut env = tilo_test_env::ScopedEnv::new();
        let _identities = quiet_refresh_sources(&mut env);
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        let downloads = root.path().join("Downloads");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&downloads).unwrap();
        for i in 0..110 {
            if i % 5 == 0 {
                std::fs::create_dir(work.join(format!("project-{i:03}"))).unwrap();
            } else {
                std::fs::write(work.join(format!("notes-{i:03}.txt")), b"n").unwrap();
            }
        }
        let widget = FileExplorerWidget::new();
        widget.state().navigate(work.clone());
        assert_eq!(widget.state().entries.len(), 110);
        let _ = widget.state().refresh_metadata_if_due(ui::time::now());

        let name = "factorio-space-age_linux-arm64_2.1.23.tar.xz";
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"truncate -s "$3" "$1/$4" && mv "$1/$4" "$2/$4""#)
            .arg("sh")
            .arg(&downloads)
            .arg(&work)
            .arg(SIZE.to_string())
            .arg(name)
            .status_locked()
            .unwrap();
        assert!(status.success());

        let shown = |widget: &FileExplorerWidget| {
            widget
                .state()
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| (entry.size, entry.is_dir))
        };
        let listed = tilo_test_env::wait_for("the moved-in archive to be listed", || {
            let readable = {
                let state = widget.state();
                state.last_directory_read + metadata_refresh::change_read_gap(state.entries.len())
            };
            let _ = widget.state().refresh_metadata_if_due(readable);
            shown(&widget)
        });
        assert_eq!(
            listed,
            (SIZE, false),
            "the moved-in archive is listed as a file, at its size"
        );
        assert_eq!(widget.state().entries.len(), 111);
        assert_eq!(
            widget.state().current_path,
            work,
            "the folder was never reopened"
        );
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
        widget.chrome().nav.set_content(Box::new(scroll));
        widget.state().next_metadata_refresh = Some(deadline + std::time::Duration::from_secs(60));
        assert!(
            widget
                .next_wake_deadline()
                .is_some_and(|wake| wake <= deadline),
            "the root must not schedule later than its scroll fade"
        );
        let refresh = deadline
            .checked_sub(std::time::Duration::from_millis(1))
            .expect("deadline lies in the future");
        widget.state().next_metadata_refresh = Some(refresh);
        assert!(
            widget
                .next_wake_deadline()
                .is_some_and(|wake| wake <= refresh),
            "the root must not schedule later than its metadata refresh"
        );
    }

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
            document_id: None,
            availability: None,
            app: None,
            rich_metadata: None,
            observed: None,
        }
    }

    #[test]
    fn view_commands_are_reachable_from_keyboard_and_command_paths() {
        set_sort_command_selects_the_key_including_type();
        command_l_opens_the_editable_location_bar_without_a_pointer();
        shortcut_reference_is_a_keyboard_and_command_reachable_view();
        zoom_commands_step_the_item_size_and_clamp();
        toggle_hidden_reveals_and_hides_dotfiles();
    }

    fn set_sort_command_selects_the_key_including_type() {
        let w = FileExplorerWidget::new();
        w.state().entries = vec![
            ent("b.png", false, "image/png"),
            ent("a.txt", false, "text/plain"),
        ];
        assert!(w.state().apply_command(FileCommand::SetSort(SortBy::Type)));
        assert_eq!(w.state().sort_by(), SortBy::Type);
        assert_eq!(
            w.state().entries_snapshot()[0].name,
            "b.png",
            "image/png < text/plain"
        );

        assert!(w.state().apply_command(FileCommand::SetSort(SortBy::Name)));
        assert_eq!(w.state().sort_by(), SortBy::Name);
        assert_eq!(w.state().entries_snapshot()[0].name, "a.txt");
    }

    fn command_l_opens_the_editable_location_bar_without_a_pointer() {
        use winit::keyboard::{Key, ModifiersState};

        let mut widget = FileExplorerWidget::new();
        let event = ui::synthetic_input::key_event(
            Key::Character(winit::keyboard::SmolStr::new("l")),
            None,
            winit::event::ElementState::Pressed,
        );

        assert!(widget.with_state(|state, chrome| state.handle_app_key(
            chrome,
            &event,
            winit::event::Modifiers::from(ModifiersState::CONTROL),
        )));
        assert!(
            widget.state().toolbar.is_path_editing(),
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
        assert!(widget.with_state(|state, chrome| state.handle_app_key(
            chrome,
            &event,
            winit::event::Modifiers::default()
        )));
        let panel = widget
            .state()
            .collab_panel
            .as_ref()
            .expect("shortcut reference opens")
            .clone();
        assert_eq!(panel.title, "Keyboard shortcuts");
        assert!(
            panel.lines.iter().any(|line| line == "Rename: F2"),
            "{:?}",
            panel.lines
        );
        let new_folder = match ui::platform::PlatformConventions::default().platform() {
            ui::platform::Platform::MacOs => "New folder: ⌘⇧N",
            _ => "New folder: Ctrl+Shift+N",
        };
        assert!(
            panel.lines.iter().any(|line| line == new_folder),
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
        w.state().current_path = trash;
        w.state().entries = vec![ent("deleted.txt", false, "text/plain")];
        w.state().selection.select_only(0);
        w.state().open_context_menu(Point::new(5.0, 5.0));
        let labels: Vec<String> = w
            .chrome()
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

    fn resource_catalogue_exposes_view_and_favourite_commands_through_the_action_api() {
        let state_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let folder = home_dir.path().join("agent-folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("b.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::with_persistence(
            Some(state_dir.path()),
            Some(home_dir.path().to_path_buf()),
        );
        w.state().navigate(folder);
        let initial_zoom = w.state().zoom_level();
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
                "{command} is routed through the canonical api"
            );
        }
        assert_eq!(w.state().view_mode(), ViewMode::Grid);
        assert_eq!(w.state().sort_by(), SortBy::Size);
        assert!(w.state().zoom_level() > initial_zoom);
        assert_eq!(
            w.state()
                .collab_panel
                .as_ref()
                .map(|panel| panel.title.as_str()),
            Some("Keyboard shortcuts"),
            "the resource action opens the same reference as the menu and keyboard"
        );
    }

    fn large_text_view_menu_clamps_inside_the_live_window() {
        let w = FileExplorerWidget::new();
        w.state().rect = vello::kurbo::Rect::new(0.0, 0.0, 1440.0, 900.0);
        let mut menu = w.state().new_overlay_menu();
        menu.push(MenuItem::action(t("files-menu-remove-favourite"), || {}));
        menu.open_at([1428.0, 52.0]);
        menu.set_text_scale(2.0);
        let menu = menu.get_rect();
        let window = w.state().rect;
        assert!(menu.x0 >= window.x0 && menu.x1 <= window.x1, "{menu:?}");
        assert!(menu.y0 >= window.y0 && menu.y1 <= window.y1, "{menu:?}");
        assert!(
            menu.width() >= 400.0,
            "the scaled menu keeps enough width for its label: {menu:?}"
        );
    }

    tilo_test_env::scenario! {
        fn cloud_nearby_sharing_and_history_commands_take_their_actions;

        phase one_time_cloud_import_is_an_independent_copy_through_the_action_api;
        {
            for kind in [
                crate::remote_accounts::RemoteAccountKind::Dropbox,
                crate::remote_accounts::RemoteAccountKind::GoogleDrive,
            ] {
                one_time_cloud_import_for_kind(kind);
            }
        }

        phase cloud_import_retains_local_destination_after_alias_changes;
        {
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
            let row = app
                .with_surface(|files: &mut FileExplorerWidget| {
                    files.state().home_path = Some(home.path().to_path_buf());
                    files.state().startup_accounts = Some(vec![account]);
                    files.state().navigate(mount.clone());
                    files.state().dispatcher = Some(ui::framework::testing::dispatcher::<
                        crate::resource::FileExplorer,
                    >("files"));
                    files
                        .state()
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
                    assert_eq!(files.state().awaiting_ops.len(), 1);
                    files.state().awaiting_ops[0].0.clone()
                })
                .unwrap();
            std::fs::remove_file(&alias).unwrap();
            std::os::unix::fs::symlink(&mount, &alias).unwrap();
            app.invoke(crate::resource::FileExplorer::queueing("test-device", op).unwrap())
                .unwrap();
            tilo_test_env::wait_until("the queued operation to settle", || {
                app.settle();
                app.state().ops.pending().next().is_none()
            });
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

        phase a_cloud_account_needing_sign_in_is_visible_and_says_what_to_do;
        {
            let account = remote_account(
                crate::remote_accounts::RemoteAccountHealth::NeedsAuthentication,
                None,
            );
            let w = FileExplorerWidget::new();
            w.state().debug_show_remote_account(&account, true);
            let ids: Vec<String> = w
                .state()
                .current_places()
                .sections()
                .iter()
                .flat_map(|section| section.items.iter().map(|item| item.id.clone()))
                .collect();
            assert!(
                ids.iter().any(|id| id == "account:cloud"),
                "the account has a row even though nothing is mounted: {ids:?}"
            );
            let panel = w
                .state()
                .collab_panel
                .as_ref()
                .expect("the sheet is open")
                .clone();
            assert_eq!(panel.title, "Sign in to Cloud");
            assert!(panel.summary.is_empty(), "no subtitle under the heading");
            let text = panel.lines.join(" ");
            assert!(!text.to_lowercase().contains("vault"), "{text}");
            assert!(!text.contains("Remote account details"), "{text}");
            assert_eq!(
                w.state().collab_panel_action_label().as_deref(),
                Some("Check again")
            );
            assert_eq!(
                w.state().collab_panel_action_command(),
                Some(FileCommand::RecheckRemoteAccount("cloud".into()))
            );
            w.state().close_collab_panel();
            assert!(
                w.state()
                    .apply_command(FileCommand::ShowRemoteAccount("cloud".into()))
            );
            assert!(w.state().collab_panel.is_some());
        }

        phase nearby_status_action_reports_progress_and_terminal_phases_without_authority;
        {
            use ui::collaboration::{
                NearbyTransferDirection, NearbyTransferHandle, NearbyTransferPhase,
                NearbyTransferSnapshot, NearbyTransferStatus,
            };

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
                    .state()
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
                let Some(encoded) = status.encode_immediate() else {
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
                .expect("read status through the production effect api") else {
                    panic!("status must return an immediate structured answer")
                };
                assert_eq!(
                    output.value,
                    Some(serde_json::json!({"transfer": expected}))
                );
                assert!(
                    widget.state().nearby_send.take_requests().is_empty(),
                    "inspection grants no authority or network work"
                );
                widget
                    .state()
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
                assert!(widget.state().nearby_send.take_requests().is_empty());
            }
        }

        phase nearby_recovered_receive_decline_is_available_without_a_destination;
        {
            use ui::collaboration::{
                CollabRequest, NearbyTransferCommand, NearbyTransferDirection, NearbyTransferHandle,
                NearbyTransferPhase, NearbyTransferSnapshot, NearbyTransferStatus,
            };

            let transfer = NearbyTransferHandle::from_issued_bytes([0x61; 16]);
            let (mut widget, _config) = hermetic();
            widget
                .state()
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
            let Some(encoded) = decline.encode_immediate() else {
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
            assert!(
                matches!(widget.state().nearby_send.take_requests().as_slice(), [
                CollabRequest::NearbyTransferCommand {
                    command: NearbyTransferCommand::Decide {
                        handle, accept: false, destination: None, remember_for_session: false,
                    }, ..
                }
            ] if *handle == transfer)
            );
        }

        phase availability_policy_commands_write_only_eligible_resources;
        {
            #[cfg(unix)]
            collaborative_selection_offers_and_applies_keep_offline();
            #[cfg(unix)]
            free_local_space_is_offered_and_sent_only_for_eligible_local_files();
            #[cfg(unix)]
            mixed_selection_exposes_both_storage_policies_and_writes_only_eligible_resources();
            #[cfg(unix)]
            inherited_pin_is_explained_but_not_offered_as_a_local_toggle();
            failed_projected_file_offers_retry_and_availability_details();
        }

        phase sharing_history_and_identity_surfaces_use_governed_identity;
        {
            sharing_offers_the_shells_invite_sheet();
            governed_history_surfaces_open_real_detail_panels();
            running_files_reloads_new_renamed_removed_and_expired_governed_devices();
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
                files.state().home_path = Some(home.path().to_path_buf());
                files.state().user_dirs = favorites::UserDirs {
                    downloads: Some(destination.clone()),
                    ..Default::default()
                };
                files.state().startup_accounts = Some(vec![account]);
                files.state().navigate(mount.clone());
                files
                    .state()
                    .published_rows()
                    .into_iter()
                    .find(|row| row.label == "Folder")
                    .unwrap()
            })
            .unwrap();
        assert!(row.actions.iter().any(|action| action.id == "import-once"));
        app.call_on_finished(
            row.id.as_str(),
            "import-once",
            serde_json::json!({"folder": destination}),
        )
        .unwrap();
        assert!(app.state().ops.pending().next().is_none());
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
                .state()
                .displayed_entries()
                .iter()
                .position(|entry| entry.name == "Folder")
                .unwrap();
            files.state().select(Some(index));
            assert!(
                files
                    .state()
                    .context_menu_spec(&[])
                    .iter()
                    .all(|(label, _)| !label.contains("Downloads")),
                "cloud import never chooses a destination on the person's behalf"
            );
        })
        .unwrap();
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

    fn projected_availability_controls_are_discoverable_and_use_the_entry_route() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("shared.txt");
        std::fs::write(&path, b"x").unwrap();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = path;
        entry.availability = Some(fs_protocol::AvailabilityProjectionV1 {
            schema_version: fs_protocol::projection::AVAILABILITY_PROJECTION_VERSION,
            document_id: fs_protocol::FileDocumentId(uuid::Uuid::from_u128(1)),
            current_hash: Some(fs_protocol::ContentHash([2; 32])),
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
        w.state().current_path = d.path().to_path_buf();
        w.state().entries = vec![entry];
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
        assert!(
            w.state().collab_panel.is_some(),
            "availability route opens details"
        );
    }

    fn trash_actions_require_an_explicit_irrevocable_confirmation() {
        let Some(trash) = crate::fileops::trash_dir() else {
            return;
        };
        let w = FileExplorerWidget::new();
        w.state().current_path = trash;
        w.state().entries = vec![ent("deleted.txt", false, "text/plain")];
        w.state().selection.select_only(0);

        assert!(w.state().apply_command(FileCommand::Delete));
        let panel = w
            .state()
            .collab_panel
            .as_ref()
            .expect("permanent delete warning")
            .clone();
        assert!(matches!(
            panel.action.as_ref(),
            Some(CollabPanelAction::PermanentDelete(_))
        ));
        assert_eq!(
            w.state().collab_panel_action_command(),
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

        w.state().close_collab_panel();
        assert!(w.state().apply_command(FileCommand::EmptyTrash));
        assert!(matches!(
            w.state()
                .collab_panel
                .as_ref()
                .and_then(|panel| panel.action.as_ref()),
            Some(CollabPanelAction::EmptyTrash)
        ));
        assert_eq!(
            w.state().collab_panel_action_command(),
            Some(FileCommand::ConfirmEmptyTrash),
            "emptying Trash must use a separate explicit action"
        );
    }

    fn context_menu_exposes_the_sort_section_including_type() {
        let mut w = FileExplorerWidget::new();
        w.state().entries = vec![ent("a.txt", false, "text/plain")];
        w.state().open_context_menu(Point::new(5.0, 5.0));
        let labels: Vec<String> = w
            .chrome()
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

    fn state_surface_copy_names_the_folder_and_offers_what_can_help() {
        let w = FileExplorerWidget::new();
        w.state().current_path = PathBuf::from("/workspace/projects/secret");
        let denied = std::io::Error::from_raw_os_error(13).to_string();
        w.state().error = Some(t_args(
            "files-directory-read-failed",
            &[arg("error", denied)],
        ));

        let (title, detail, has_action) = w.state().state_surface_copy().expect("error state copy");
        assert_eq!(
            title,
            t_args("files-load-error-title", &[arg("name", "secret")])
        );
        assert_eq!(detail, t("files-folder-error-permission"));
        assert!(has_action);
        assert_eq!(
            w.state().folder_recovery_command(),
            FileCommand::Up,
            "trying again can't fix a permission; going up can"
        );
        assert_eq!(recovery_label(&w), t("files-open-enclosing"));

        let odd = std::io::Error::from_raw_os_error(5).to_string();
        w.state().error = Some(t_args("files-directory-read-failed", &[arg("error", odd)]));
        let (_, detail, _) = w.state().state_surface_copy().expect("error state copy");
        assert!(!detail.contains("os error"), "{detail}");
        assert_eq!(w.state().folder_recovery_command(), FileCommand::Refresh);
        assert_eq!(recovery_label(&w), t("files-try-again"));
    }

    fn recovery_label(w: &FileExplorerWidget) -> String {
        let pane = w.state().build_content_pane(&ui::theme::Theme::dark());
        let state = pane.accessibility().expect("the failure is a state");
        let [action] = state.children.as_slice() else {
            panic!("a failure offers one way on: {state:?}");
        };
        action.name.clone().unwrap_or_default()
    }

    fn retry_refresh_recovers_from_error_once_the_dir_exists() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("appears-later");
        let w = FileExplorerWidget::new();
        w.state().navigate(target.clone());
        assert!(
            w.state().error_message().is_some(),
            "missing dir sets an error"
        );
        assert!(w.state().apply_command(FileCommand::Refresh));
        assert!(w.state().error_message().is_some());
        std::fs::create_dir(&target).unwrap();
        assert!(w.state().apply_command(FileCommand::Refresh));
        assert!(
            w.state().error_message().is_none(),
            "retry succeeds once the dir exists"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_lists_as_what_it_opens() {
        let d = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let folder = elsewhere.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("inside.txt"), b"x").unwrap();
        let file = elsewhere.path().join("file.txt");
        std::fs::write(&file, vec![b'x'; 5000]).unwrap();
        std::fs::write(d.path().join("a-plain.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(&folder, d.path().join("z-linked-folder")).unwrap();
        std::os::unix::fs::symlink(&file, d.path().join("linked-file.txt")).unwrap();
        std::os::unix::fs::symlink("missing-target", d.path().join("dangling")).unwrap();

        let w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
        let shown = w.state().displayed_entries();
        let names: Vec<&str> = shown.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "z-linked-folder",
                "a-plain.txt",
                "dangling",
                "linked-file.txt"
            ],
            "the linked folder sorts with the folders"
        );
        let linked_folder = &shown[0];
        assert!(linked_folder.is_dir, "a link to a folder is a folder");
        let linked_file = shown
            .iter()
            .find(|entry| entry.name == "linked-file.txt")
            .unwrap();
        assert!(!linked_file.is_dir);
        assert_eq!(
            linked_file.size, 5000,
            "a linked file shows its target's size"
        );
        let dangling = shown.iter().find(|entry| entry.name == "dangling").unwrap();
        assert!(!dangling.is_dir, "a link to nothing stays a file");
        let link_meta = std::fs::symlink_metadata(d.path().join("z-linked-folder")).unwrap();
        assert_eq!(
            linked_folder.observed,
            host_fs::ObservedObject::from_metadata(&link_meta)
        );
        let answer = w.state().folder_answer().to_string();
        assert!(
            answer.contains("\"z-linked-folder/\""),
            "an agent reads the linked folder as a folder: {answer}"
        );
    }

    #[test]
    fn rename_and_creation_flows_change_disk_names_locally() {
        rename_flow_renames_the_selected_file_on_disk();
        reseeding_the_rename_field_never_mutates_the_shared_document();
        rename_commit_is_a_noop_for_blank_or_unchanged_names();
        rename_cancel_leaves_the_file_untouched();
        exported_rename_changes_case_without_overwriting_another_entry();
        new_folder_creates_the_folder_and_opens_inline_rename();
        new_from_template_creates_the_file_and_opens_rename();
        new_folders_name_typed_in_a_burst_keeps_every_character();
        a_new_folders_name_typed_before_its_field_opens_is_kept();
        super::headless_f2_renames_fixture_without_opening_a_file();
    }

    fn a_new_folders_name_typed_before_its_field_opens_is_kept() {
        // The press has happened and the row's field is on its way: the person types the name at
        // once, and Enter with it. None of it may read as choosing a row.
        let widget = FileExplorerWidget::new();
        widget.state().naming_ahead = Some(super::NamingAhead::default());
        widget.state().awaiting_ops.push((
            crate::file_ops::FileOp::NewFolder {
                parent: "/".to_owned(),
                name: "New folder".to_owned(),
            },
            super::OpFollowUp::NewFolder,
        ));
        let mut driver =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(widget), (1280, 800));
        driver.type_text("Walks");
        driver.key("Enter");
        let widget = driver
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<FileExplorerWidget>())
            .expect("the Files window");
        let ahead = widget.state().naming_ahead.take().expect("the name waited");
        assert_eq!(ahead.typed, "Walks", "every character typed is kept");
        assert!(ahead.commit, "Enter with it asks for it to be made");
    }

    fn new_folders_name_typed_in_a_burst_keeps_every_character() {
        let d = tempfile::tempdir().unwrap();
        let widget = FileExplorerWidget::new();
        widget.state().navigate(d.path().to_path_buf());
        assert!(
            widget.state().apply_command(FileCommand::NewFolder),
            "New folder runs"
        );
        assert!(
            widget.state().is_renaming(),
            "the new folder asks for its name"
        );
        let mut driver =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(widget), (1280, 800));
        // The name typed as one burst, Enter in the same breath.
        driver.type_text("Walks");
        driver.key("Enter");
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            d.path().join("Walks").is_dir(),
            "the folder takes every character typed: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "New folder"),
            "the proposed name is not what is kept: {names:?}"
        );
    }

    fn rename_flow_renames_the_selected_file_on_disk() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("old.txt"), b"x").unwrap();
        let w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
        let idx = w
            .state()
            .displayed_entries()
            .iter()
            .position(|e| e.name == "old.txt")
            .unwrap();
        w.state().select(Some(idx));

        assert!(w.state().apply_command(FileCommand::Rename));
        assert!(w.state().is_renaming());
        assert_eq!(w.state().rename_buffer(), "old.txt");

        w.state().set_rename_buffer("new.txt".into());
        assert!(w.state().apply_command(FileCommand::RenameCommit));
        assert!(!w.state().is_renaming(), "commit closes the rename bar");
        assert!(d.path().join("new.txt").exists(), "file renamed on disk");
        assert!(!d.path().join("old.txt").exists());
    }

    fn reseeding_the_rename_field_never_mutates_the_shared_document() {
        // An in-progress filename is device-local interaction state. Repeated
        // renames must not create an ever-growing CRDT history, leak a path to
        // members, or cause a remote client to see a half-typed name.
        let d = tempfile::tempdir().unwrap();
        for i in 0..40 {
            std::fs::write(d.path().join(format!("report-{i:03}.txt")), b"x").unwrap();
        }
        let mut w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());

        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let baseline = doc.save();
        w.state().set_rename_buffer("report-000.txt".into());
        w.sync(&doc, &root);

        for i in 1..40 {
            w.state().set_rename_buffer(format!("report-{i:03}.txt"));
            w.sync(&doc, &root);
        }
        assert_eq!(
            w.state().rename_buffer(),
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
        let w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
        let idx = w
            .state()
            .displayed_entries()
            .iter()
            .position(|e| e.name == "keep.txt")
            .unwrap();

        w.state().select(Some(idx));
        w.state().begin_rename();
        w.state().set_rename_buffer("   ".into());
        assert!(!w.state().commit_rename(), "blank name does not rename");
        assert!(!w.state().is_renaming());
        assert!(d.path().join("keep.txt").exists());

        w.state().select(Some(idx));
        w.state().begin_rename();
        w.state().set_rename_buffer("keep.txt".into());
        assert!(!w.state().commit_rename(), "unchanged name does not rename");
        assert!(d.path().join("keep.txt").exists());
    }

    fn rename_cancel_leaves_the_file_untouched() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("stay.txt"), b"x").unwrap();
        let w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
        let idx = w
            .state()
            .displayed_entries()
            .iter()
            .position(|e| e.name == "stay.txt")
            .unwrap();
        w.state().select(Some(idx));
        w.state().apply_command(FileCommand::Rename);
        w.state().set_rename_buffer("other.txt".into());
        assert!(w.state().apply_command(FileCommand::RenameCancel));
        assert!(!w.state().is_renaming());
        assert!(d.path().join("stay.txt").exists());
        assert!(!d.path().join("other.txt").exists());
    }

    fn new_folder_creates_the_folder_and_opens_inline_rename() {
        let d = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
        assert!(w.state().apply_command(FileCommand::NewFolder));
        assert!(
            d.path().join("New folder").is_dir(),
            "folder actually created"
        );
        assert!(
            w.state().is_renaming(),
            "new folder drops straight into rename"
        );
        assert_eq!(w.state().rename_buffer(), "New folder");
    }

    fn resource_open_refuses_a_deleted_or_replayed_entry_target() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("transient.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
        let mut doc = automerge::AutoCommit::new();
        let open = crate::effects::FilesEffect::Open {
            item: crate::search_provider::entry_resource_id(&file),
        }
        .bound()
        .build_json(&serde_json::Map::new())
        .unwrap();

        std::fs::remove_file(&file).unwrap();
        w.state().refresh();
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
        let w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
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
        let w = FileExplorerWidget::new();
        w.state().current_path = d.path().to_path_buf();
        w.state().entries = (0..=MAX_EAGER_ENTRY_RESOURCES)
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

        let last = w
            .state()
            .entries
            .last()
            .expect("fixture has a final entry")
            .clone();
        let id = FilesState::entry_resource_id(&last);
        let resolved = w
            .state()
            .published_row(&id)
            .expect("an exact current-folder entry resolves on demand");
        assert_eq!(resolved.id(), id);
        let ops = resolved.operations();
        assert!(ops.iter().any(|action| action.id == "open"));
        assert!(ops.iter().any(|action| action.id == "copy"));
    }

    fn every_exported_local_file_handler_is_declared_for_the_broker() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("notes.txt"), b"txt").unwrap();
        let files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
        files.state().navigate(root.path().to_path_buf());
        let resources = crate::files_app::published::tree(&files, &automerge::AutoCommit::new());
        let registry = ui::tilo_actions::ActionRegistry::with(
            <crate::resource::FileExplorer as ui::tilo_resource::Resource>::register,
        );
        let mut handlers = ui::widget::resource_operation_handlers(&files);
        handlers.insert(
            <crate::effects::FilesEffect as ui::tilo_resource::Effect>::OPERATION.to_owned(),
        );
        let violations = ui::tilo_actions::validate_surface(&resources, &registry, &handlers);
        assert!(violations.is_empty(), "{violations:?}");
    }

    fn exported_rename_changes_case_without_overwriting_another_entry() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("report.txt");
        std::fs::write(&original, b"source content").unwrap();
        let occupied = directory.path().join("occupied.txt");
        std::fs::write(&occupied, b"keep occupied content").unwrap();
        let files = FileExplorerWidget::with_persistence(None, Some(directory.path().into()));
        files.state().navigate(directory.path().into());
        let invoke_rename = |source: &Path, name: &str| -> Result<bool, String> {
            files.state().refresh();
            let index = files
                .state()
                .displayed_entries()
                .iter()
                .position(|entry| entry.path == source)
                .ok_or_else(|| "not listed".to_string())?;
            files.state().selection.select_only(index);
            files.state().begin_rename();
            files.state().set_rename_buffer(name.into());
            files.state().commit_rename();
            if files.state().is_renaming() {
                files.state().cancel_rename();
                return Err(files.state().status.clone().unwrap_or_default());
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
        use ui::tilo_actions::{ActionAddress, api};

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
        let document_id = uuid::Uuid::from_u128(0xfeed);
        let w = FileExplorerWidget::new();
        w.state().workspace_roots = vec![d.path().to_path_buf()];
        w.state().current_path = d.path().to_path_buf();
        let mut entry = ent("old\u{1f}.txt", false, "text/plain");
        entry.path = old.clone();
        entry.document_id = Some(*document_id.as_bytes());
        w.state().entries = vec![entry];
        let mut doc = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(&w, &doc);
        let entry = resources[0]
            .children
            .iter()
            .find(|resource| {
                resource.id.as_str() == crate::search_provider::resource_resource_id(document_id)
            })
            .expect("a collaborative file uses its resource UUID as the resource identity");
        assert_eq!(entry.kind, "files.record");
        let address = entry
            .action_address("copy")
            .expect("a collaborative file publishes copy");
        assert_eq!(
            ActionAddress::parse(&address.encode()),
            Some(address.clone())
        );
        assert!(api::resolve(&resources, &address).is_some());
        w.state().selection.select_only(0);
        w.state().begin_rename();
        w.state().set_rename_buffer("new.txt".into());
        w.state().commit_rename();
        let _ = &mut doc;
        assert!(!old.exists());
        let renamed = d.path().join("new.txt");
        assert!(renamed.exists());
        w.state().entries[0].path = renamed;
        w.state().entries[0].name = "new.txt".into();
        w.state().entries[0].document_id = Some(*document_id.as_bytes());
        let refreshed = crate::files_app::published::tree(&w, &doc);
        assert!(
            api::resolve(&refreshed, &address).is_some(),
            "the resource action address survives a path rename and resolves its fresh path"
        );
    }

    fn exported_background_operations_have_valid_job_contracts() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("shared.txt"), b"x").unwrap();
        let w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
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

    fn copy_then_paste_duplicates_the_file_into_the_current_dir() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let w = FileExplorerWidget::new();
        w.state().navigate(src.clone());
        let idx = w
            .state()
            .displayed_entries()
            .iter()
            .position(|e| e.name == "a.txt")
            .unwrap();
        w.state().select(Some(idx));
        assert!(w.state().apply_command(FileCommand::Copy));
        w.state().navigate(dest.clone());
        assert!(w.state().apply_command(FileCommand::Paste));
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

        let w = FileExplorerWidget::new();
        w.state().navigate(src.clone());
        let idx = w
            .state()
            .displayed_entries()
            .iter()
            .position(|e| e.name == "m.txt")
            .unwrap();
        w.state().select(Some(idx));
        assert!(w.state().apply_command(FileCommand::Cut));
        w.state().navigate(dest.clone());
        assert!(w.state().apply_command(FileCommand::Paste));
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

        let w = FileExplorerWidget::new();
        w.state().navigate(src);
        {
            let mut state = w.state();
            let count = state.displayed_entries().len();
            state.selection.select_all(count);
        }
        assert!(w.state().apply_command(FileCommand::Cut));
        std::fs::remove_file(&vanished).unwrap();

        w.state().navigate(dest.clone());
        assert!(w.state().apply_command(FileCommand::Paste));

        assert!(dest.join("moved.txt").exists(), "the available item moved");
        assert_eq!(
            w.state().clipboard,
            vec![vanished],
            "retry must contain only the item whose move actually failed"
        );
        assert!(
            w.state().clipboard_cut,
            "the remaining item keeps cut semantics"
        );
    }

    fn invoke_transfer_action(
        files: &mut FileExplorerWidget,
        source: Option<&Path>,
        action: &str,
    ) -> Result<bool, String> {
        use ui::tilo_actions::api;
        let mut doc = automerge::AutoCommit::new();
        let resources = crate::files_app::published::tree(files, &doc);
        let resource = source.map_or_else(
            || resources.first().unwrap().id.as_str().to_owned(),
            crate::search_provider::entry_resource_id,
        );
        let address = api::published(&resources, &resource, action)
            .expect("the current resource must export the transfer action");
        let encoded = api::encode_json(&resources, &address, &serde_json::Map::new())
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
            files.state().navigate(directory.path().into());
            assert!(
                invoke_transfer_action(&mut files, Some(&source), if cut { "cut" } else { "copy" })
                    .unwrap()
            );
            files.state().navigate(destination.clone());
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
            assert_eq!(files.state().clipboard.is_empty(), cut);
            assert!(
                files
                    .state()
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
        files.state().navigate(directory.path().into());
        assert!(invoke_transfer_action(&mut files, Some(&source), "cut").unwrap());
        files.state().navigate(destination.clone());
        std::fs::remove_dir(&destination).unwrap();
        std::fs::write(&destination, b"replacement bytes").unwrap();
        assert!(invoke_transfer_action(&mut files, None, "paste").is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"selected bytes");
        assert_eq!(std::fs::read(&destination).unwrap(), b"replacement bytes");
        assert_eq!(files.state().clipboard, vec![source.clone()]);
        assert!(files.state().clipboard_cut);

        std::fs::remove_file(&destination).unwrap();
        std::fs::create_dir(&destination).unwrap();
        assert!(invoke_transfer_action(&mut files, None, "paste").unwrap());
        assert_eq!(
            std::fs::read(destination.join("report.txt")).unwrap(),
            b"selected bytes"
        );
        assert!(!source.exists());
        assert!(files.state().clipboard.is_empty());
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);
    }

    fn history_back_and_forward_traverse_visited_dirs() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir(&a).unwrap();
        let b = d.path().join("b");
        std::fs::create_dir(&b).unwrap();

        let w = FileExplorerWidget::new();
        w.state().navigate(d.path().to_path_buf());
        w.state().navigate(a.clone());
        w.state().navigate(b.clone());
        assert_eq!(w.state().current_path(), b);
        w.state().history_back();
        assert_eq!(
            w.state().current_path(),
            a,
            "back to the previously-visited dir"
        );
        w.state().history_back();
        assert_eq!(w.state().current_path(), d.path());
        w.state().history_forward();
        assert_eq!(w.state().current_path(), a, "forward re-visits");
    }

    fn hermetic() -> (FileExplorerWidget, tempfile::TempDir) {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path()),
            Some(PathBuf::from("/home/tester")),
        );
        (w, cfg)
    }

    /// A file offers sending to a nearby device once: greyed, saying why, while none is near, and
    /// with each device online as a choice, read by its name, once one is.
    #[test]
    #[cfg(unix)]
    fn a_file_offers_sending_nearby_once() {
        use ui::collaboration::{
            CollabEvent, CollabRequest, NearbyTargetHandle, NearbyTransferCommand,
            NearbyTransferReply, NearbyTransferSnapshot, NearbyTransferTarget,
        };
        use ui::tilo_actions::api::spec_of;
        let (widget, config) = hermetic();
        let source = config.path().join("report.txt");
        std::fs::write(&source, b"to send").unwrap();
        widget.state().navigate(config.path().to_path_buf());
        let document = automerge::AutoCommit::new();
        let entry = crate::search_provider::entry_resource_id(&source);

        let resources = crate::files_app::published::tree(&widget, &document);
        let picker = spec_of(&resources, &entry, "send-nearby").expect("one offer with none near");
        assert_eq!(picker.label, t("files-menu-send-nearby"));
        assert!(matches!(
            &picker.contract.availability,
            ui::tilo_actions::Availability::Unavailable { reason, .. }
                if *reason == t("files-nearby-no-devices")
        ));
        assert!(spec_of(&resources, &entry, "send-to").is_none());

        widget
            .state()
            .nearby_send
            .handle_event(&CollabEvent::NearbyTransferChanged);
        let request_id = widget
            .state()
            .nearby_send
            .take_requests()
            .iter()
            .find_map(|request| match request {
                CollabRequest::NearbyTransferCommand {
                    request_id,
                    command: NearbyTransferCommand::Snapshot,
                } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        let handle = NearbyTargetHandle::from_issued_bytes([7; 16]);
        widget
            .state()
            .nearby_send
            .handle_event(&CollabEvent::NearbyTransferCommandResult {
                request_id,
                result: Ok(NearbyTransferReply::Snapshot {
                    snapshot: NearbyTransferSnapshot {
                        targets: vec![NearbyTransferTarget {
                            handle,
                            label: "Kitchen tablet".into(),
                            online: true,
                        }],
                        ..NearbyTransferSnapshot::default()
                    },
                }),
            });
        let resources = crate::files_app::published::tree(&widget, &document);
        assert!(spec_of(&resources, &entry, "send-nearby").is_none());
        let send = spec_of(&resources, &entry, "send-to").expect("one offer with a device near");
        assert_eq!(send.label, t("files-menu-send-nearby"));
        let target = send
            .params()
            .iter()
            .find(|param| &*param.name == "target")
            .unwrap();
        assert_eq!(
            target.choice_label(&hex::encode([7u8; 16])),
            Some("Kitchen tablet")
        );
    }

    /// The surface the session's catalogue is built from says what each row offers with a device
    /// near: the row's `send-to` names every device online as a choice of its target, read by the
    /// device's name, and a value outside them binds nothing. An agent or a script picks the
    /// device from here, as a person does in the row's menu.
    #[test]
    #[cfg(unix)]
    fn a_published_row_offers_each_nearby_device_as_a_send_to_choice() {
        use ui::collaboration::{
            CollabEvent, CollabRequest, NearbyTargetHandle, NearbyTransferCommand,
            NearbyTransferReply, NearbyTransferSnapshot, NearbyTransferTarget,
        };
        fn find<'a>(
            rows: &'a [ui::tilo_actions::ResourceSpec],
            action: &str,
        ) -> Option<&'a ui::tilo_actions::ActionSpec> {
            rows.iter().find_map(|row| {
                row.actions
                    .iter()
                    .find(|spec| spec.id == action)
                    .or_else(|| find(&row.children, action))
            })
        }
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("report.txt"), b"to send").unwrap();
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::FileExplorerShell>::for_person(
                &"fe".repeat(32),
            )
            .unwrap();
        app.with_widget(|w: &mut FileExplorerWidget| {
            w.state().navigate(directory.path().to_path_buf());
        });
        app.settle();
        app.with_widget(|w: &mut FileExplorerWidget| {
            w.state()
                .nearby_send
                .handle_event(&CollabEvent::NearbyTransferChanged);
            let request_id = w
                .state()
                .nearby_send
                .take_requests()
                .iter()
                .find_map(|request| match request {
                    CollabRequest::NearbyTransferCommand {
                        request_id,
                        command: NearbyTransferCommand::Snapshot,
                    } => Some(*request_id),
                    _ => None,
                })
                .unwrap();
            let handle = NearbyTargetHandle::from_issued_bytes([7; 16]);
            w.state()
                .nearby_send
                .handle_event(&CollabEvent::NearbyTransferCommandResult {
                    request_id,
                    result: Ok(NearbyTransferReply::Snapshot {
                        snapshot: NearbyTransferSnapshot {
                            targets: vec![NearbyTransferTarget {
                                handle,
                                label: "Kitchen tablet".into(),
                                online: true,
                            }],
                            ..NearbyTransferSnapshot::default()
                        },
                    }),
                });
        });
        app.settle();
        let resources = app.harness().resources();
        let send = find(&resources, "send-to")
            .expect("a row offers sending to the device the surface names");
        let target = send
            .params()
            .iter()
            .find(|param| param.name == "target")
            .expect("the device is the action's target");
        let device = hex::encode([7u8; 16]);
        assert_eq!(
            target
                .choices
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![device.clone()],
            "the device online is the one choice"
        );
        assert_eq!(
            target.choice_label(&device),
            Some("Kitchen tablet"),
            "the choice reads as the device's name"
        );
        let mut refused = serde_json::Map::new();
        refused.insert("target".into(), "not-a-device".into());
        assert!(
            send.build_json(&refused).is_err(),
            "a value the surface did not offer binds nothing"
        );
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
        widget.state().navigate(config.path().to_path_buf());
        let mut document = automerge::AutoCommit::new();
        let entry = crate::search_provider::entry_resource_id(&source);
        let resources = crate::files_app::published::tree(&widget, &document);
        assert!(ui::tilo_actions::api::spec_of(&resources, &entry, "send-air-drop").is_none());
        widget
            .state()
            .nearby_send
            .handle_event(&CollabEvent::NearbyTransferChanged);
        let request_id = widget
            .state()
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
            .state()
            .nearby_send
            .handle_event(&CollabEvent::NearbyTransferCommandResult {
                request_id,
                result: Ok(NearbyTransferReply::AirDropAvailability { available: true }),
            });
        let resources = crate::files_app::published::tree(&widget, &document);
        let action = ui::tilo_actions::api::spec_of(&resources, &entry, "send-air-drop")
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
        let requests = widget.state().nearby_send.take_requests();
        assert!(
            matches!(requests.as_slice(), [CollabRequest::NearbyTransferCommand {
            command: NearbyTransferCommand::SendAirDrop { source_path, address, port: 443 }, ..
        }] if source_path == source.as_os_str().as_encoded_bytes() && address == "127.0.0.1")
        );
        std::fs::remove_file(&source).unwrap();
        widget.state().refresh();
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
        assert!(widget.state().nearby_send.take_requests().is_empty());
    }

    fn nearby_receiver_actions_are_opaque_confirmed_and_current() {
        use ui::collaboration::{
            NearbyDestinationHandle, NearbyTransferDestination, NearbyTransferDirection,
            NearbyTransferHandle, NearbyTransferPhase, NearbyTransferSnapshot,
            NearbyTransferStatus,
        };

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
            .state()
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
            .find(|action| action.id == "accept-nearby")
            .expect("accept action");
        assert_eq!(accept.contract.confirmation, ConfirmationPolicy::Confirm);
        assert!(
            !accept.id.contains("42424242"),
            "the destination is an argument, not an id"
        );
        let mut accepted = serde_json::Map::new();
        accepted.insert(
            "destination".into(),
            hex::encode(destination.as_bytes()).into(),
        );
        let encoded = accept
            .build_json(&accepted)
            .expect("the destination this window holds binds");

        snapshot.transfers.clear();
        widget
            .state()
            .nearby_send
            .install_snapshot_for_test(snapshot);
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
        assert!(widget.state().nearby_send.take_requests().is_empty());
    }

    fn double_click_click_command_navigates_into_a_directory() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let (w, _cfg) = hermetic();
        w.state().navigate(d.path().to_path_buf());
        let idx = w
            .state()
            .displayed_entries()
            .iter()
            .position(|e| e.name == "sub")
            .unwrap();

        w.state().apply_command(FileCommand::Click {
            index: idx,
            ctrl: false,
            shift: false,
        });
        assert_eq!(
            w.state().current_path(),
            d.path(),
            "first click only selects"
        );
        assert_eq!(w.state().selected_index(), Some(idx));

        w.state().apply_command(FileCommand::Click {
            index: idx,
            ctrl: false,
            shift: false,
        });
        assert_eq!(
            w.state().current_path(),
            d.path().join("sub"),
            "double-click opens the directory"
        );
    }

    fn zoom_commands_step_the_item_size_and_clamp() {
        let (w, _cfg) = hermetic();
        let base = w.state().zoom_level();
        w.state().apply_command(FileCommand::ZoomIn);
        assert_eq!(w.state().zoom_level(), base + 1, "zoom in raises the step");
        w.state().apply_command(FileCommand::ZoomOut);
        w.state().apply_command(FileCommand::ZoomOut);
        assert_eq!(w.state().zoom_level(), base - 1, "zoom out lowers the step");
        for _ in 0..50 {
            w.state().apply_command(FileCommand::ZoomOut);
        }
        assert_eq!(
            w.state().zoom_level(),
            config::ZOOM_MIN,
            "zoom clamps at the minimum"
        );
    }

    fn favorite_commands_add_reorder_and_remove() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path()),
            Some(PathBuf::from("/home/tester")),
        );
        let sub = PathBuf::from("/home/tester/Projects/tilo");
        let before = w.state().favorites().len();
        w.state()
            .apply_command(FileCommand::AddFavorite(sub.clone()));
        assert_eq!(
            w.state().favorites().len(),
            before + 1,
            "added one favourite"
        );
        assert_eq!(w.state().favorites().last().unwrap().path, sub);

        let last = w.state().favorites().len() - 1;
        w.state()
            .apply_command(FileCommand::ReorderFavorite { from: last, to: 1 });
        assert_eq!(
            w.state().favorites()[1].path,
            sub,
            "reorder moved it to slot 1"
        );

        w.state()
            .apply_command(FileCommand::RemoveFavorite(sub.clone()));
        assert!(!w.state().favorites().iter().any(|f| f.path == sub));
    }

    fn favourites_follow_localized_xdg_user_dirs() {
        use crate::favorites::{FavKind, UserDirs};
        let cfg = tempfile::tempdir().unwrap();
        let home = PathBuf::from("/home/tester");
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
        let labels: Vec<String> = w
            .state()
            .favorites()
            .iter()
            .map(|f| f.label.clone())
            .collect();
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
        let docs = w
            .state()
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

        let (w, _cfg) = hermetic();
        w.state().templates = crate::templates::Catalog::new(
            Some(d.path().to_path_buf()),
            ui::WakeHandle::detached(),
        );
        w.state().navigate(work.clone());
        w.state().apply_command(FileCommand::NewFromTemplate(tmpl));
        assert!(
            work.join("Doc.md").exists(),
            "template copied into the current dir"
        );
        assert_eq!(
            std::fs::read(work.join("Doc.md")).unwrap(),
            b"template body"
        );
        assert!(
            w.state().is_renaming(),
            "new file drops straight into inline-rename"
        );
    }

    fn toggle_hidden_reveals_and_hides_dotfiles() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".secret"), b"x").unwrap();
        std::fs::write(d.path().join("visible.txt"), b"x").unwrap();
        let (w, _cfg) = hermetic();
        w.state().navigate(d.path().to_path_buf());
        assert!(
            !w.state()
                .entries_snapshot()
                .iter()
                .any(|e| e.name == ".secret"),
            "dotfiles hidden by default"
        );
        w.state().apply_command(FileCommand::ToggleHidden);
        assert!(w.state().show_hidden());
        assert!(
            w.state()
                .entries_snapshot()
                .iter()
                .any(|e| e.name == ".secret"),
            "dotfiles shown after toggle"
        );
        w.state().apply_command(FileCommand::ToggleHidden);
        assert!(
            !w.state()
                .entries_snapshot()
                .iter()
                .any(|e| e.name == ".secret"),
            "toggled back off"
        );
    }

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

        let (w, _cfg) = hermetic();
        w.state().navigate(d.path().to_path_buf());

        assert_eq!(
            w.state().error_message(),
            None,
            "a directory that reads cleanly reports no error",
        );

        let mut listed: Vec<String> = w
            .state()
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

        let (count, _path) = w.state().footer_text();
        assert_eq!(
            count,
            names.len(),
            "the footer counts the same entries the list shows",
        );
    }

    fn scroll_position_survives_a_structural_rebuild_but_resets_on_navigation() {
        // Selecting a row or re-sorting rebuilds the whole composition with a
        // fresh `Scrollable`. The persisted offset cell, the one the rebuilt
        // `Scrollable` binds to through `with_persisted_offset`, must carry the
        // position across those rebuilds and reset only when the directory
        // changes.
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir(&a).unwrap();
        for i in 0..6 {
            std::fs::write(a.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let b = d.path().join("b");
        std::fs::create_dir(&b).unwrap();

        let (w, _cfg) = hermetic();
        w.state().navigate(a);
        w.state().scroll_offset.set((0.0, 240.0));

        w.state().apply_command(FileCommand::Click {
            index: 0,
            ctrl: false,
            shift: false,
        });
        assert!(
            (w.state().scroll_offset.get().1 - 240.0).abs() <= f32::EPSILON,
            "selecting a row keeps the scroll"
        );
        w.state().apply_command(FileCommand::SetSort(SortBy::Size));
        assert!(
            (w.state().scroll_offset.get().1 - 240.0).abs() <= f32::EPSILON,
            "re-sorting keeps the scroll"
        );

        w.state().navigate(b);
        assert_eq!(
            w.state().scroll_offset.get(),
            (0.0, 0.0),
            "a new directory scrolls to the top"
        );
    }

    fn context_select_targets_the_clicked_row_not_a_stale_selection() {
        // Guards the dangerous bug: a right-click's menu must act on the row it
        // landed on, not whatever happened to be selected. `ContextSelect` is
        // the command the content pane pushes on a right-press.
        let (w, _cfg) = hermetic();
        w.state().entries = vec![
            ent("a.txt", false, "text/plain"),
            ent("b.txt", false, "text/plain"),
            ent("c.txt", false, "text/plain"),
        ];
        w.state().selection.select_only(0);
        assert!(w.state().apply_command(FileCommand::ContextSelect(Some(2))));
        assert_eq!(
            w.state().selected_indices(),
            vec![2],
            "context select targets the clicked row"
        );

        w.state().selection.select_only(1);
        w.state().selection.toggle(2);
        w.state().apply_command(FileCommand::ContextSelect(Some(1)));
        let mut got = w.state().selected_indices();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![1, 2],
            "right-click within a multi-selection keeps it"
        );

        w.state().apply_command(FileCommand::ContextSelect(None));
        assert!(
            w.state().selected_indices().is_empty(),
            "empty-space right-click clears selection"
        );
    }

    fn a_screen_reader_opens_a_rows_menu_on_the_row_it_names() {
        let (w, _cfg) = hermetic();
        w.state().entries = vec![
            ent("a.txt", false, "text/plain"),
            ent("b.txt", false, "text/plain"),
            ent("c.txt", false, "text/plain"),
        ];
        w.state().selection.select_only(0);
        assert!(w.state().apply_command(FileCommand::ContextMenuAt {
            index: 2,
            at: Point::new(120.0, 200.0),
        }));
        assert_eq!(
            w.state().selected_indices(),
            vec![2],
            "the menu acts on the row the reader named"
        );
        assert!(
            w.state().opened_menu().is_some(),
            "and the window's one menu opened"
        );
    }

    fn context_menu_checks_new_file_on_templates_existing() {
        let (w, _cfg) = hermetic();
        let has_new_file = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label.starts_with("New file"))
        };
        let has_new_folder = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label == "New folder")
        };

        let none = w.state().context_menu_spec(&[]);
        assert!(!has_new_file(&none), "no templates → submenu hidden");
        assert!(has_new_folder(&none), "New Folder is always offered");

        let two = w
            .state()
            .context_menu_spec(&[PathBuf::from("Doc.md"), PathBuf::from("Sheet.csv")]);
        assert!(
            has_new_file(&two),
            "templates present → New File entry shown"
        );
        assert!(has_new_folder(&two));
    }

    #[cfg(unix)]
    fn collaborative_selection_offers_and_applies_keep_offline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shared.txt");
        std::fs::write(&file, b"shared").unwrap();
        let availability = serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "document_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_hash": null,
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

        let w = FileExplorerWidget::new();
        w.state().current_path = dir.path().to_path_buf();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = file.clone();
        entry.availability = crate::model::read_availability(&file);
        w.state().entries = vec![entry];
        w.state().selection.select_only(0);
        let spec = w.state().context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Keep offline" && *command == Some(FileCommand::KeepOffline)
        }));
        assert!(w.state().apply_command(FileCommand::KeepOffline));
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
                "schema_version": 2,
                "document_id": uuid::Uuid::new_v4(),
                "current_hash": null,
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

        let widget = FileExplorerWidget::new();
        widget.state().current_path = dir.path().to_path_buf();
        widget.state().entries = [("local.txt", &local), ("remote.txt", &remote)]
            .into_iter()
            .map(|(name, path)| {
                let mut entry = ent(name, false, "text/plain");
                entry.path = path.clone();
                entry.availability = crate::model::read_availability(path);
                entry
            })
            .collect();
        widget.state().selection.select_all(2);

        assert!(
            widget
                .state()
                .context_menu_spec(&[])
                .iter()
                .any(|(label, command)| {
                    label == "Free local space" && *command == Some(FileCommand::FreeLocalSpace)
                })
        );
        assert!(widget.state().apply_command(FileCommand::FreeLocalSpace));
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
    fn scheduled_metadata_refresh_preserves_resource_selection_and_scroll() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ROOTS", dir.path());
        let before = dir.path().join("before.txt");
        let after = dir.path().join("after.txt");
        std::fs::write(&before, b"shared").unwrap();
        let record = uuid::Uuid::from_u128(0x5151);
        xattr::set(&before, ui::prelude::DOCUMENT_ID_XATTR, record.as_bytes()).unwrap();
        xattr::set(&before, fs_protocol::SYNC_XATTR, b"projected").unwrap();

        let widget = FileExplorerWidget::new();
        widget.state().load(dir.path().to_path_buf());
        // A workspace mount is FUSE: its watch reports local edits only, and
        // remote ones (renames, sync state) are found by the poll.
        widget.state().directory_watch.assume_partial_coverage();
        widget.state().selection.select_only(0);
        widget.state().scroll_offset.set((0.0, 137.0));
        std::fs::rename(&before, &after).unwrap();
        xattr::set(&after, fs_protocol::SYNC_XATTR, b"local").unwrap();
        widget.state().next_metadata_refresh = Some(ui::time::now());

        assert!(widget.state().refresh_metadata_if_due(ui::time::now()));
        assert_eq!(widget.state().entries[0].path, after);
        assert_eq!(
            widget.state().entries[0].sync_state,
            Some(fs_protocol::SyncState::Local)
        );
        assert_eq!(widget.state().selection.selected(), vec![0]);
        assert_eq!(widget.state().selection.primary(), Some(0));
        assert_eq!(widget.state().scroll_offset.get(), (0.0, 137.0));
    }

    #[cfg(unix)]
    fn mixed_selection_exposes_both_storage_policies_and_writes_only_eligible_resources() {
        let dir = tempfile::tempdir().unwrap();
        let automatic = dir.path().join("automatic.txt");
        let pinned = dir.path().join("pinned.txt");
        let ordinary = dir.path().join("ordinary.txt");
        for path in [&automatic, &pinned, &ordinary] {
            std::fs::write(path, b"data").unwrap();
        }
        let availability = |policy: &str, inherited: bool| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "document_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_hash": null,
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

        let w = FileExplorerWidget::new();
        w.state().current_path = dir.path().to_path_buf();
        w.state().entries = [
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
        w.state().selection.select_all(3);

        let spec = w.state().context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Keep offline" && *command == Some(FileCommand::KeepOffline)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Use automatic storage" && *command == Some(FileCommand::RemoveOfflinePin)
        }));

        w.state().apply_command(FileCommand::KeepOffline);
        assert_eq!(
            xattr::get(&automatic, fs_protocol::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice())
        );
        assert_eq!(xattr::get(&pinned, fs_protocol::PIN_XATTR).unwrap(), None);
        assert_eq!(xattr::get(&ordinary, fs_protocol::PIN_XATTR).unwrap(), None);

        for entry in &mut w.state().entries {
            entry.availability = crate::model::read_availability(&entry.path);
        }
        {
            let mut state = w.state();
            let count = state.entries.len();
            state.selection.select_all(count);
        }
        w.state().apply_command(FileCommand::RemoveOfflinePin);
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
            "the opposite policy action must not rewrite an automatic resource"
        );
        assert_eq!(xattr::get(&ordinary, fs_protocol::PIN_XATTR).unwrap(), None);
    }

    #[cfg(unix)]
    fn inherited_pin_is_explained_but_not_offered_as_a_local_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("inherited.txt");
        std::fs::write(&file, b"data").unwrap();
        let availability = serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "document_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_hash": null,
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

        let w = FileExplorerWidget::new();
        w.state().current_path = dir.path().to_path_buf();
        let mut entry = ent("inherited.txt", false, "text/plain");
        entry.path = file.clone();
        entry.availability = crate::model::read_availability(&file);
        w.state().entries = vec![entry];
        w.state().selection.select_only(0);

        let spec = w.state().context_menu_spec(&[]);
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
        w.state().apply_command(FileCommand::RemoveOfflinePin);
        assert_eq!(xattr::get(&file, fs_protocol::PIN_XATTR).unwrap(), None);
    }

    fn failed_projected_file_offers_retry_and_availability_details() {
        let mut entry = ent("offline.txt", false, "text/plain");
        entry.availability = fs_protocol::AvailabilityProjectionV1::parse_xattr(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "document_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_hash": "4242424242424242424242424242424242424242424242424242424242424242",
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
        let w = FileExplorerWidget::new();
        w.state().entries = vec![entry];
        w.state().selection.select_only(0);

        let spec = w.state().context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Retry download" && *command == Some(FileCommand::RetryTransfer)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Availability…" && *command == Some(FileCommand::ShowAvailability)
        }));

        let entry = w.state().entries[0].clone();
        let mut resource = w.state().entry_resource(&entry).into_resource();
        ui::framework::apply_row_statuses(std::slice::from_mut(&mut resource), &|id| {
            w.state().row_status(id)
        });
        let presentation = resource.presentation.as_ref().expect("unavailable badge");
        assert_eq!(presentation.badge.as_deref(), Some("Unavailable"));
        assert!(presentation.is_valid());
        let open = resource
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("open action");
        assert_eq!(
            open.description.as_deref(),
            Some("Unavailable until a device that keeps it is online")
        );

        w.state().apply_command(FileCommand::ShowAvailability);
        let panel = w
            .state()
            .collab_panel
            .as_ref()
            .expect("availability panel")
            .clone();
        assert!(panel.summary.contains("Not stored"));
        assert!(panel.lines.iter().any(|line| line.contains("other device")));
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line == "Unavailable until a device that keeps it is online"),
            "{:?}",
            panel.lines
        );
        assert!(
            !panel
                .lines
                .iter()
                .any(|line| line.contains("No available replica")),
            "the daemon's own wording never reaches the person: {:?}",
            panel.lines
        );
    }

    fn adas_directory() -> (crate::identity_directory::TrustedHumanDirectory, [u8; 32]) {
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let person = root_key.verifying_key().to_bytes();
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: person,
            device_public_key: ed25519_dalek::SigningKey::from_bytes(&[92; 32])
                .verifying_key()
                .to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root_key).unwrap();
        let directory = crate::identity_directory::TrustedHumanDirectory::from_projection_v2(
            tilo_user_identity::TrustedIdentityProjectionV2 {
                schema_version: tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
                humans: vec![tilo_user_identity::GovernedHumanIdentityV2 {
                    root_public_key: person,
                    display_name: "Ada Lovelace".to_string(),
                    devices: vec![tilo_user_identity::GovernedDeviceIdentityV2 {
                        certificate,
                        agent: None,
                    }],
                }],
            },
        )
        .unwrap();
        (directory, person)
    }

    fn sharing_offers_the_shells_invite_sheet() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("shared-assets");
        std::fs::create_dir(&folder).unwrap();
        let (directory, _person) = adas_directory();
        let mut w = FileExplorerWidget::new();
        w.state().workspace_roots = vec![root.path().to_path_buf()];
        w.state().trusted_humans = directory;
        let mut entry = ent("shared-assets", true, "inode/directory");
        entry.path = folder;
        w.state().entries = vec![entry];
        w.state().selection.select_only(0);

        let spec = w.state().context_menu_spec(&[]);
        let sharing: Vec<_> = spec
            .iter()
            .filter(|(label, _)| label.starts_with("Share") || label.contains("invite link"))
            .collect();
        assert_eq!(
            sharing,
            [&("Share…".to_owned(), Some(FileCommand::Share))],
            "one sharing row, and it is the shell's sheet"
        );
        let at = spec
            .iter()
            .position(|(label, _)| label == "Share…")
            .expect("the sharing row");
        assert_eq!(
            spec.get(at - 1),
            Some(&(String::new(), None)),
            "sharing opens a group of its own, a separator above it"
        );
        assert_eq!(
            spec.get(at + 1).map(|(label, _)| label.as_str()),
            Some("Properties…"),
            "the group stands directly before the file's properties, and the \
             menu joins a one-row group to the one it opens"
        );

        assert!(w.state().apply_command(FileCommand::Share));
        let request = w
            .take_collab_requests()
            .into_iter()
            .find_map(|request| match request {
                ui::collaboration::CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                } => Some(request),
                _ => None,
            })
            .expect("the sheet is asked of the shell");
        assert_eq!(request.target.provider, "so.tilo.session");
        assert_eq!(request.target.address.resource, "session:invite");
        assert_eq!(request.target.address.action, "show-invite");
        assert_eq!(
            request.argument,
            ui::tilo_actions::remote::InvokeArgument::Json(serde_json::Map::new()),
            "the workspace in front is the one the person is looking at"
        );
        ui::widget::Collaborative::handle_collab_event(
            &mut w,
            &ui::collaboration::CollabEvent::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Receipt(
                    tilo_actions::remote::InvocationReceipt {
                        invocation: request.invocation,
                        status: tilo_actions::remote::ReceiptStatus::Completed {
                            output: tilo_actions::ActionOutput::default(),
                        },
                    },
                ),
            },
        );
        assert_eq!(
            w.state().status,
            None,
            "the sheet says what became of it; Files says nothing more"
        );
    }

    fn governed_history_surfaces_open_real_detail_panels() {
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.document_id =
            Some(*uuid::Uuid::from_u128(0xb0d4_e23f_c7cb_4ded_a427_b32c_f9da_9341).as_bytes());
        entry.rich_metadata = fs_protocol::MetadataProjectionV1::parse_xattr(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "document_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "properties": {"schema_version": 1, "image_width": 1920,
                    "image_height": 1080, "duration_ms": 65000,
                    "page_count": 4, "codec": "h264", "container": "mp4"}
            }))
            .unwrap(),
        )
        .ok();
        let w = FileExplorerWidget::new();
        w.state().entries = vec![entry];
        w.state().selection.select_only(0);
        let spec = w.state().context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Version history…" && *command == Some(FileCommand::ShowHistory)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Properties…" && *command == Some(FileCommand::ShowProperties)
        }));
        w.state().apply_command(FileCommand::ShowProperties);
        let panel = w.state().collab_panel.as_ref().unwrap().clone();
        assert!(panel.title.contains("Properties"));
        let internal = |line: &String| line.contains("Collaborative resource");
        assert!(!panel.lines.iter().any(internal), "{:?}", panel.lines);
        assert!(panel.lines.iter().any(|line| line.contains("1920 × 1080")));
        assert!(panel.lines.iter().any(|line| line.contains("1:05")));
        assert!(panel.lines.iter().any(|line| line.contains("Pages: 4")));
        w.state().apply_command(FileCommand::ShowHistory);
        assert!(w.state().collab_panel.is_none());
        w.state().apply_command(FileCommand::CloseCollabPanel);
        assert!(w.state().collab_panel.is_none());
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
                        issuer: None,
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
        let widget = FileExplorerWidget::new();

        std::fs::write(
            &path,
            serde_json::to_vec(&projection(&root, "Ada", &[(94, "Laptop", None)])).unwrap(),
        )
        .unwrap();
        assert!(widget.state().refresh_trusted_identities(Some(&path)));
        assert_eq!(
            widget
                .state()
                .trusted_humans
                .human(&person)
                .unwrap()
                .devices
                .len(),
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
        assert!(widget.state().refresh_trusted_identities(Some(&path)));
        let human = widget
            .state()
            .trusted_humans
            .human(&person)
            .unwrap()
            .clone();
        assert_eq!(human.display_name, "Ada Renamed");
        assert_eq!(human.devices.len(), 2);

        std::fs::write(
            &path,
            serde_json::to_vec(&projection(&root, "Ada Renamed", &[(95, "Phone", Some(2))]))
                .unwrap(),
        )
        .unwrap();
        assert!(widget.state().refresh_trusted_identities(Some(&path)));
        assert!(
            widget
                .state()
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
        assert!(widget.state().refresh_trusted_identities(Some(&path)));
        assert!(widget.state().trusted_humans.humans().is_empty());
    }

    #[cfg(unix)]
    fn collaborative_search_activation_resolves_the_latest_path_by_document_id() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::from_u128(0xabc);
        let nested = dir.path().join("nested");
        let file = nested.join("renamed.txt");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&file, b"hello").unwrap();
        xattr::set(&file, ui::prelude::DOCUMENT_ID_XATTR, id.as_bytes()).unwrap();
        crate::model::sync_state_tests::set_resource_index(
            dir.path(),
            &[serde_json::json!({
                "document_id": id,
                "path": "/nested/renamed.txt",
                "is_dir": false
            })],
        );
        let widget = FileExplorerWidget::new();
        widget.state().current_path = dir.path().to_path_buf();
        widget.state().entries.clear();
        widget.state().workspace_roots = vec![dir.path().to_path_buf()];
        {
            let mut state = widget.state();
            state.collaborative_resource_pager = RefCell::new(
                crate::model::WorkspaceDocumentIndexPager::new(&state.workspace_roots),
            );
        }
        widget.state().refresh_external_search_catalogs();

        let source = ui::framework::testing::search_source::<crate::resource::FileExplorer>(
            widget.state().search_entries_shown(),
        );
        let hit = ui::search::SearchProvider::search(&source, "renamed")
            .into_iter()
            .find(|result| result.action_id == crate::search_provider::resource_resource_id(id))
            .expect("collaborative resource is searchable");
        let row = widget
            .state()
            .published_row(&hit.anchor())
            .expect("late resource materializes exactly for this anchor");
        assert!(
            row.operations()
                .iter()
                .any(|spec| spec.id == crate::search_provider::REVEAL_ACTION),
            "the anchored row reveals the resource"
        );
        widget
            .state()
            .open_anchor(&hit.anchor())
            .expect("opening the anchor reveals the resource");
        assert_eq!(widget.state().current_path, nested);
        assert_eq!(widget.state().selection.primary(), Some(0));
        assert_eq!(widget.state().entries[0].path, file);
    }

    fn archives_browse_as_read_only_folders_and_extract_from_inside() {
        let _home = tilo_test_env::TestHome::new().expect("private test home");
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
        files.state().navigate(archive);
        files.state().wait_for_archive();
        assert_eq!(files.state().entries_snapshot().len(), 2);
        assert!(
            files
                .state()
                .entries_snapshot()
                .iter()
                .any(|entry| entry.name == "Photos" && entry.is_dir)
        );
        assert!(
            files
                .state()
                .entries_snapshot()
                .iter()
                .any(|entry| entry.name == "readme.txt" && !entry.is_dir)
        );

        let photos = files
            .state()
            .entries_snapshot()
            .iter()
            .position(|entry| entry.name == "Photos")
            .unwrap();
        session_api::start();
        asking_session(&files);
        files.state().open_entry(photos).unwrap();
        assert_eq!(files.state().entries_snapshot().len(), 1);
        assert_eq!(files.state().entries_snapshot()[0].name, "one.jpg");
        assert!(files.state().open_entry(0).unwrap());
        files.state().wait_for_archive_members();
        settle_opens(&files);
        let copy = session_api::opened()
            .pop()
            .expect("the member's copy is opened");
        assert_eq!(std::fs::read(&copy).unwrap(), b"one");

        files.state().open_view_menu();
        let menu_labels: Vec<_> = files
            .chrome()
            .context_menu
            .items()
            .iter()
            .filter_map(|item| item.label())
            .collect();
        assert!(menu_labels.contains(&"Extract here"));
        assert!(menu_labels.contains(&"Extract to…"));
        assert!(menu_labels.iter().all(|label| !label.starts_with('✓')));
        assert!(!menu_labels.contains(&"New folder"));
        assert!(!menu_labels.contains(&"Open terminal here"));
        let list_choice = files
            .chrome()
            .context_menu
            .items()
            .iter()
            .find(|item| item.label() == Some("List"))
            .unwrap();
        assert_eq!(list_choice.checked(), Some(true));

        files.state().selection.select_only(0);
        assert!(files.state().apply_command(FileCommand::Delete));
        assert!(source.join("Photos/one.jpg").exists());
        let read_only = t("files-archive-read-only");
        assert_eq!(files.state().status_line(), Some(read_only.as_str()));

        assert!(
            files
                .state()
                .apply_command(FileCommand::ExtractCurrentArchive)
        );
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
#[path = "responsive_body_tests.rs"]
mod responsive_body_tests;

#[cfg(test)]
mod accessibility_tests {
    use super::*;
    use ui::a11y::{Politeness, Role};

    fn empty_widget() -> (tempfile::TempDir, FileExplorerWidget) {
        let home = tempfile::tempdir().unwrap();
        let widget = FileExplorerWidget::with_persistence(None, Some(home.path().to_path_buf()));
        widget.state().entries.clear();
        widget.state().error = None;
        widget.state().is_loading = false;
        (home, widget)
    }

    #[test]
    fn files_states_and_modal_sheets_publish_accessible_semantics() {
        custom_empty_loading_and_error_states_are_live_semantics();
        deterministic_shot_boundaries_cover_loading_and_actionable_error_states();
        collaboration_sheet_is_a_focused_modal_semantic_subtree();
        #[cfg(unix)]
        destructive_eviction_requires_a_named_modal_action_before_writing_the_override();
    }

    fn custom_empty_loading_and_error_states_are_live_semantics() {
        let (_home, widget) = empty_widget();
        let empty = Widget::accessibility(&widget).expect("Files has a live root");
        assert_eq!(empty.role, Role::Group);
        assert_eq!(empty.name.as_deref(), Some("Files activity"));
        assert_eq!(empty.live, Some(Politeness::Polite));
        assert_eq!(empty.value.as_deref(), Some("This folder is empty"));
        assert!(!empty.state.busy);

        widget.state().is_loading = true;
        let loading = Widget::accessibility(&widget).unwrap();
        assert!(loading.state.busy);
        assert!(loading.value.as_deref().unwrap().starts_with("Loading "));
        let theme = ui::theme::Theme::dark();
        let pane = widget.state().build_content_pane(&theme);
        let state = pane
            .accessibility()
            .expect("the pane says the folder is opening");
        assert_eq!(state.name.as_deref(), Some("Opening folder"));
        assert_eq!(state.live, Some(Politeness::Polite));
        assert!(state.state.busy);

        widget.state().is_loading = false;
        widget.state().error = Some("permission denied".into());
        let error = Widget::accessibility(&widget).unwrap();
        assert_eq!(error.live, Some(Politeness::Assertive));
        assert!(
            error
                .value
                .as_deref()
                .unwrap()
                .contains("permission denied")
        );
        let pane = widget.state().build_content_pane(&theme);
        let state = pane.accessibility().expect("error state semantic group");
        let name = widget
            .state()
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
        assert_eq!(retry.action_target.as_deref(), Some(RETRY_ROW));
    }

    fn deterministic_shot_boundaries_cover_loading_and_actionable_error_states() {
        let (_home, widget) = empty_widget();
        widget.state().debug_show_loading_state();
        assert!(widget.state().is_loading());
        assert!(widget.state().error().is_none());
        assert!(widget.state().entries().is_empty());

        widget.state().debug_show_error_state();
        assert!(!widget.state().is_loading());
        assert!(
            widget
                .state()
                .error()
                .is_some_and(|error| error.contains("Permission denied"))
        );
        assert!(widget.state().entries().is_empty());
    }

    fn collaboration_sheet_is_a_focused_modal_semantic_subtree() {
        let (_home, widget) = empty_widget();
        widget.state().show_collab_panel(CollabPanel {
            title: "Share report.pdf".into(),
            summary: "Shared with 2 people".into(),
            lines: vec!["Alice can view".into(), "Bob can edit".into()],
            action: None,
        });

        let dialog = Widget::accessibility(&widget).expect("open sheet has semantics");
        assert_eq!(dialog.role, Role::Dialog);
        assert_eq!(dialog.name.as_deref(), Some("Share report.pdf"));
        assert!(dialog.state.modal && dialog.state.focusable && dialog.state.focused);
        assert_eq!(dialog.children[0].role, Role::Heading { level: 2 });
        assert_eq!(dialog.children[1].role, Role::Paragraph);
        assert!(dialog.value.as_deref().unwrap().contains("Bob can edit"));
        assert!(Widget::traps_focus(&widget));
        assert!(
            Widget::children(&widget).is_empty(),
            "background toolbar/sidebar/list leave the semantic tree while modal"
        );

        widget.state().close_collab_panel();
        assert!(!Widget::traps_focus(&widget));
        assert!(!Widget::children(&widget).is_empty());
        assert_eq!(Widget::accessibility(&widget).unwrap().role, Role::Group);
    }

    #[cfg(unix)]
    fn destructive_eviction_requires_a_named_modal_action_before_writing_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("only-copy.txt");
        std::fs::write(&file, b"only local copy").unwrap();
        let (_home, widget) = empty_widget();
        widget.state().show_collab_panel(CollabPanel {
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
            .state()
            .collab_panel_action_command()
            .expect("the explicit action is keyboard/click routable");
        widget.state().close_collab_panel();
        assert!(widget.state().apply_command(command));
        assert_eq!(
            xattr::get(&file, fs_protocol::AVAILABILITY_CONTROL_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"evict-destructive".as_slice())
        );
    }
}

#[cfg(test)]
#[path = "breadcrumb_home_tests.rs"]
mod breadcrumb_home_tests;

pub(super) fn with_owner_callback(callback: impl FnOnce() + 'static) {
    use std::cell::RefCell;
    use ui::widget::Widget;

    struct Probe<F>(RefCell<Option<F>>);
    impl<F: FnOnce() + 'static> ui::resources::PublishesResources for Probe<F> {
        fn resources(&self, _: &automerge::AutoCommit) -> Vec<ui::tilo_actions::ResourceSpec> {
            assert!(ui::bound_dispatch::local_person_invocation_permitted());
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

    fn startup(path: &Path) -> StartupInputs {
        StartupInputs {
            initial_dir: path.to_path_buf(),
            workspace_roots: Vec::new(),
            resource_roots: Vec::new(),
            remote_accounts: Vec::new(),
            trusted_humans: crate::identity_directory::TrustedHumanDirectory::default(),
        }
    }

    fn absent_services() -> HostedFileServices {
        HostedFileServices {
            thumbnails: None,
            local_clipboard: false,
        }
    }

    fn owned_child(parent: &Path) -> PathBuf {
        let child = parent.join("files");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("Welcome.txt"), b"Shell capture fixture\n").unwrap();
        child
    }

    tilo_test_env::scenario! {
        fn injected_startup_shows_accounts_and_uses_only_host_inputs;

        phase a_remote_account_is_a_folder_with_a_health_badge;
        {
            with_owner_callback(remote_account_folder_with_health_badge);
        }

        phase injected_startup_uses_only_explicit_host_inputs_and_services;
        {
            injected_startup_and_reload_keep_the_host_directory_and_catalogs();
            injected_services_absence_never_builds_thumbnails_or_reaches_past_the_session();
            injected_listing_and_refresh_use_only_explicit_workspace_roots();
            fixture_io_guards_stop_before_following_io();
        }
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
            widget.state().current_path,
            mount,
            "opening navigates into the folder"
        );
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
        assert_eq!(widget.state().current_path, child);
        assert_eq!(widget.state().displayed_entries().len(), 1);
        assert_eq!(widget.state().displayed_entries()[0].name, "Welcome.txt");
        ui::widget::Widget::set_focus(&mut widget, true);
        let document = automerge::AutoCommit::new();
        ui::widget::Widget::sync(&mut widget, &document, &automerge::ROOT);
        widget.state().refresh_external_search_catalogs();
        assert!(!widget.state().refresh_trusted_identities(None));
        widget
            .state()
            .refresh_metadata_if_due(ui::time::now() + metadata_refresh::LIVE_INTERVAL);
        widget.state().refresh();
        assert_eq!(widget.state().current_path, child);
        assert!(widget.state().workspace_roots.is_empty());
        assert_eq!(widget.state().remote_account_snapshot(), vec![account]);
        assert_eq!(
            widget.state().trusted_humans,
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

    fn injected_services_absence_never_builds_thumbnails_or_reaches_past_the_session() {
        let parent = tempfile::tempdir().unwrap();
        let child = owned_child(parent.path());
        let guard = DenyAmbientIo::new();
        let listings = OwnedListings::new(&child);
        let widget = FileExplorerWidget::with_startup_inputs_and_services(
            startup(&child),
            absent_services(),
        );
        session_api::start();
        asking_session(&widget);
        assert!(widget.state().thumbs.is_none());
        crate::command::push(&widget.state().cmds, FileCommand::Activate(0));
        assert!(widget.state().drain_commands());
        settle_opens(&widget);
        assert_eq!(session_api::opened(), vec![child.join("Welcome.txt")]);
        assert_eq!(widget.state().notice_line(), None);
        assert_eq!(guard.attempts(), 0);
        assert_eq!(listings.observations(), vec![(child, false)]);
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
        let widget =
            FileExplorerWidget::with_startup_inputs_and_services(inputs, absent_services());
        std::fs::write(child.join("Reload.txt"), b"reload").unwrap();
        assert!(widget.state().apply_command(FileCommand::Refresh));
        assert_eq!(widget.state().displayed_entries().len(), 2);
        std::fs::write(child.join("Metadata.txt"), b"metadata").unwrap();
        assert!(widget.state().refresh_metadata_preserving_view());
        assert_eq!(widget.state().displayed_entries().len(), 3);
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
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("report.txt");
    std::fs::write(&original, b"preserved bytes").unwrap();
    let widget = FileExplorerWidget::with_persistence(None, None);
    asking_session(&widget);
    session_api::start();
    widget.state().navigate(directory.path().to_path_buf());
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
            session_api::opened().is_empty(),
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

    tilo_test_env::scenario! {
        fn sorting_keeps_selection_rename_anchor_and_clipboard_on_the_same_files;

        phase sorting_keeps_an_active_rename_on_the_original_file;
        {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("a.txt"), b"aaa").unwrap();
            std::fs::write(root.path().join("b.txt"), b"b").unwrap();
            let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
            files.state().navigate(root.path().into());
            files.state().set_sort_by(SortBy::Name);
            files
                .state()
                .apply_command(FileCommand::SetSortOrder(SortOrder::Ascending));
            files.state().apply_command(FileCommand::Click {
                index: 0,
                ctrl: false,
                shift: false,
            });
            files.state().apply_command(FileCommand::Rename);
            files.state().set_rename_buffer("chosen.txt".into());
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
            assert!(files.state().is_renaming());
            assert!(files.state().rename_input.is_focused());
            assert_eq!(files.state().rename_buffer, "chosen.txt");
            assert!(files.state().commit_rename());
            assert_eq!(
                std::fs::read(root.path().join("chosen.txt")).unwrap(),
                b"aaa"
            );
            assert_eq!(std::fs::read(root.path().join("b.txt")).unwrap(), b"b");
            assert!(!root.path().join("a.txt").exists());
        }

        phase every_published_sort_keeps_selection_and_an_empty_selection_stays_empty;
        {
            let root = tempfile::tempdir().unwrap();
            for (name, bytes) in [("a.txt", "aaa"), ("b.md", "b"), ("c.txt", "cc")] {
                std::fs::write(root.path().join(name), bytes).unwrap();
            }
            let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
            files.state().navigate(root.path().into());
            let selected_path = root.path().join("a.txt");
            let index = files
                .state()
                .entries
                .iter()
                .position(|entry| entry.path == selected_path)
                .unwrap();
            files.state().apply_command(FileCommand::Click {
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
                    files.state().selected_paths(),
                    vec![selected_path.clone()],
                    "{action}"
                );
                let state = files.state();
                assert_eq!(
                    state.entries[state.selection.primary().unwrap()].path,
                    selected_path
                );
                assert_eq!(
                    state.entries[state.selection.anchor().unwrap()].path,
                    selected_path
                );
            }
            files.state().selection.clear();
            files.state().toggle_sort_order();
            assert!(files.state().selection.is_empty());
            assert_eq!(files.state().selection.primary(), None);
            assert_eq!(files.state().selection.anchor(), None);
        }

        phase sorting_keeps_the_range_anchor_and_a_deselected_keyboard_cursor;
        {
            let root = tempfile::tempdir().unwrap();
            for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
                std::fs::write(root.path().join(name), name).unwrap();
            }
            let files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
            files.state().navigate(root.path().into());
            files.state().set_sort_by(SortBy::Name);
            files
                .state()
                .apply_command(FileCommand::SetSortOrder(SortOrder::Ascending));
            files.state().apply_command(FileCommand::Click {
                index: 0,
                ctrl: true,
                shift: false,
            });
            files.state().apply_command(FileCommand::Click {
                index: 2,
                ctrl: false,
                shift: true,
            });
            files.state().toggle_sort_order();
            assert_eq!(files.state().selection.anchor(), Some(3));
            assert_eq!(files.state().selection.primary(), Some(1));
            files.state().apply_command(FileCommand::Click {
                index: 2,
                ctrl: false,
                shift: true,
            });
            assert_eq!(files.state().selection.selected(), vec![2, 3]);
            files.state().apply_command(FileCommand::Click {
                index: 2,
                ctrl: true,
                shift: false,
            });
            assert!(!files.state().selection.is_selected(2));
            files.state().toggle_sort_order();
            assert_eq!(files.state().selection.selected(), vec![0]);
            assert_eq!(files.state().selection.anchor(), Some(0));
            assert_eq!(files.state().selection.primary(), Some(1));
            files.state().apply_command(FileCommand::Click {
                index: 3,
                ctrl: false,
                shift: true,
            });
            assert_eq!(files.state().selection.selected(), vec![0, 1, 2, 3]);
        }

        phase sorting_does_not_open_another_file_from_a_previous_click;
        {
            let root = tempfile::tempdir().unwrap();
            for name in ["a", "b"] {
                std::fs::create_dir(root.path().join(name)).unwrap();
            }
            let files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
            files.state().navigate(root.path().into());
            files.state().set_sort_by(SortBy::Name);
            files
                .state()
                .apply_command(FileCommand::SetSortOrder(SortOrder::Ascending));
            files.state().apply_command(FileCommand::Click {
                index: 0,
                ctrl: false,
                shift: false,
            });
            files.state().toggle_sort_order();
            files.state().apply_command(FileCommand::Click {
                index: 0,
                ctrl: false,
                shift: false,
            });
            assert_eq!(files.state().current_path, root.path());
            assert_eq!(files.state().selected_paths(), vec![root.path().join("b")]);
        }

        phase sorting_preserves_the_files_copied_by_a_multiselection;
        {
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
            files.state().navigate(source.clone());
            files.state().set_sort_by(SortBy::Name);
            files
                .state()
                .apply_command(FileCommand::SetSortOrder(SortOrder::Ascending));
            for index in [0, 2] {
                files.state().apply_command(FileCommand::Click {
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
            let mut paths = files.state().selected_paths();
            paths.sort();
            assert_eq!(paths, vec![source.join("a.txt"), source.join("c.txt")]);
            assert!(files.state().apply_command(FileCommand::Copy));
            files.state().navigate(destination.clone());
            assert!(files.state().apply_command(FileCommand::Paste));
            assert_eq!(std::fs::read(destination.join("a.txt")).unwrap(), b"aaa");
            assert_eq!(std::fs::read(destination.join("c.txt")).unwrap(), b"cc");
            assert!(!destination.join("b.txt").exists());
            assert_eq!(std::fs::read(source.join("a.txt")).unwrap(), b"aaa");
        }
    }

    #[test]
    fn sort_capture_stages_keep_the_same_selected_files() {
        use crate::resource::FileExplorerShell;
        use ui::prelude::TestApp;
        for scenario in ["selection-sort", "selection-sorted", "selection-reversed"] {
            let mut app = TestApp::<FileExplorerShell>::capture(Some(scenario)).unwrap();
            app.with_surface(|files: &mut FileExplorerWidget| {
                let mut names: Vec<_> = files
                    .state()
                    .selected_paths()
                    .iter()
                    .map(|path| path.file_name().unwrap().to_owned())
                    .collect();
                names.sort();
                assert_eq!(names, vec!["Notes.txt", "Recipes.md"]);
                assert_eq!(files.state().view_mode(), ViewMode::List);
                assert_eq!(
                    files.state().sort_by(),
                    if scenario == "selection-sort" {
                        SortBy::Name
                    } else {
                        SortBy::Size
                    }
                );
                assert!(files.state().status.is_none());
            })
            .unwrap();
        }
    }
}

#[path = "recent_resource_tests.rs"]
mod recent_resource_tests;

tilo_test_env::scenario! {
    fn window_file_work_reports_clashes_progress_previews_and_drops;

    phase completed_clipboard_undo_feedback_survives_the_actual_listing_refresh;
    {
        let home = tempfile::tempdir().unwrap();
        let widget = FileExplorerWidget::with_persistence(None, Some(home.path().to_owned()));
        widget.state().navigate(home.path().to_owned());
        widget.state().follow_ops(&ui::tilo_resource::Outbox::new());
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
        widget.state().follow_ops(&ops);
        assert_eq!(
            widget.state().status.as_deref(),
            Some(t("files-clipboard-undone").as_str())
        );
        assert!(widget.state().entries.is_empty());
    }

    phase selected_files_are_shared_as_the_items_an_agent_acts_on;
    {
        let folder = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(folder.path().join(name), name).unwrap();
        }
        let files = FileExplorerWidget::new();
        files.state().navigate(folder.path().to_path_buf());
        assert!(ui::shared_selection::selection_of(&files).is_none());
        let displayed = files.state().displayed_entries();
        let index = |name: &str| {
            displayed
                .iter()
                .position(|entry| entry.name == name)
                .unwrap()
        };
        files.state().selection.select_only(index("a.txt"));
        files.state().selection.toggle(index("c.txt"));
        let selection = ui::shared_selection::selection_of(&files).expect("two files selected");
        let ids: Vec<String> = ["a.txt", "c.txt"]
            .iter()
            .map(|name| FilesState::entry_resource_id(&displayed[index(name)]))
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
        assert_eq!(
            selection.icon,
            Some(ui::icons::Icon::FileText),
            "its chip leads with the glyph the rows show, not the Files window"
        );
    }

    phase a_name_clash_asks_once_and_each_answer_does_what_it_says;
    {
        let _home = tilo_test_env::TestHome::new().expect("private test home");
        let setup = || {
            let dir = tempfile::tempdir().unwrap();
            let (src, dest) = (dir.path().join("src"), dir.path().join("dest"));
            std::fs::create_dir_all(&src).unwrap();
            std::fs::create_dir_all(&dest).unwrap();
            std::fs::write(src.join("a.txt"), b"new").unwrap();
            std::fs::write(src.join("b.txt"), b"new").unwrap();
            std::fs::write(dest.join("a.txt"), b"old").unwrap();
            let w = FileExplorerWidget::with_persistence(None, None);
            w.state().navigate(src.clone());
            w.state()
                .drop_paths_into(&[src.join("a.txt"), src.join("b.txt")], &dest, false);
            assert!(w.state().clash.is_some(), "a taken name asks first");
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

        let (_dir, w, src, dest) = setup();
        w.state().settle_clash(Some(ClashChoice::Skip));
        assert_eq!(names(&dest), ["a.txt", "b.txt"]);
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");
        assert!(src.join("a.txt").exists(), "the skipped item stays");

        let (_dir, w, _src, dest) = setup();
        w.state().settle_clash(Some(ClashChoice::KeepBoth));
        assert_eq!(names(&dest).len(), 3, "{:?}", names(&dest));

        let (_dir, w, _src, dest) = setup();
        w.state().settle_clash(Some(ClashChoice::Replace));
        assert_eq!(names(&dest), ["a.txt", "b.txt"]);
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"new");

        let (_dir, w, src, _dest) = setup();
        w.state().settle_clash(None);
        assert!(
            w.state().clash.is_none() && src.join("a.txt").exists(),
            "Escape changes nothing"
        );
    }

    phase a_tracked_copy_reports_bytes_and_stops_on_cancel;
    {
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
        let outcome = crate::file_ops::execute_tracked(&op, None, &cancelling);
        assert_eq!(outcome.reason(), Some(crate::file_ops::CANCELLED));
        assert!(
            outcome.result().is_empty(),
            "the item it stopped in, and the ones after, are not there"
        );
    }

    phase space_previews_the_selection_and_closes_again;
    {
        use winit::keyboard::{Key, NamedKey};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"first line\nsecond").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"other").unwrap();
        let mut w = FileExplorerWidget::with_persistence(None, None);
        w.state().navigate(dir.path().to_path_buf());
        w.state().select(Some(0));
        let space = ui::synthetic_input::key_event(
            Key::Named(NamedKey::Space),
            None,
            winit::event::ElementState::Pressed,
        );
        assert!(w.with_state(|state, chrome| state.handle_app_key(
            chrome,
            &space,
            winit::event::Modifiers::default()
        )));
        assert_eq!(
            w.state().preview.as_ref().map(Preview::title),
            Some("a.txt")
        );
        let down = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            Key::Named(NamedKey::ArrowDown),
            None,
            winit::event::ElementState::Pressed,
        ));
        w.state()
            .route_preview_event(&down, winit::event::Modifiers::default());
        assert_eq!(
            w.state().preview.as_ref().map(Preview::title),
            Some("b.txt")
        );
        w.state().route_preview_event(
            &ui::synthetic_input::keyboard_input(space),
            winit::event::Modifiers::default(),
        );
        assert!(w.state().preview.is_none(), "Space closes it again");
    }

    phase files_dropped_from_another_app_are_copied_here;
    {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        for name in ["one.txt", "two.txt"] {
            std::fs::write(src.path().join(name), name.as_bytes()).unwrap();
        }
        let mut w = FileExplorerWidget::with_persistence(None, None);
        w.state().navigate(dest.path().to_path_buf());
        for name in ["one.txt", "two.txt"] {
            assert!(w.with_state(|state, chrome| state.route_incoming_drop(
                chrome,
                &winit::event::WindowEvent::DroppedFile(src.path().join(name))
            )));
        }
        w.state().take_incoming_drop();
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

    phase a_file_inside_an_archive_opens_as_a_read_only_copy;
    {
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
            .find(|entry| entry.kind == crate::archive::Kind::File)
            .expect("a file in the archive")
            .path;
        let copy = archive_member_copy(&zip, &member).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"read me");
        assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
        assert_eq!(archive_member_copy(&zip, &member).unwrap(), copy, "reused");
        let missing = archive_member_copy(&zip, std::path::Path::new("nope.txt"));
        assert!(missing.is_err());
    }

    phase opening_a_file_of_the_replaced_system_brings_a_copy_into_the_workspace;
    {
        let replaced = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let original = replaced.path().join("Notes.md");
        std::fs::write(&original, b"what the old system held").unwrap();

        let copy = crate::fileops::import_mounted_into(&original, workspace.path()).unwrap();
        assert_eq!(copy, workspace.path().join("Notes.md"));
        assert_eq!(
            std::fs::read(&copy).unwrap(),
            b"what the old system held",
            "the copy is the file's first version in the workspace"
        );

        std::fs::write(&copy, b"edited in Tilo").unwrap();
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"what the old system held",
            "the edit is the workspace's, and the original is untouched"
        );

        let again = crate::fileops::import_mounted_into(&original, workspace.path()).unwrap();
        assert_eq!(again, workspace.path().join("Notes 2.md"));
        assert_eq!(
            std::fs::read(&copy).unwrap(),
            b"edited in Tilo",
            "a second open never overwrites the copy the person edited"
        );
    }
}

#[path = "oauth_cloud_location_tests.rs"]
mod oauth_cloud_location_tests;

#[path = "person_accounts_places_tests.rs"]
mod person_accounts_places_tests;

tilo_test_env::scenario! {
    fn listings_keep_row_ids_and_read_folders_off_the_frame_loop;

    phase a_listed_entry_is_found_by_its_row_id_as_the_listing_changes;
    {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.txt"), b"a").unwrap();
        std::fs::write(directory.path().join("b.txt"), b"b").unwrap();
        let files = FileExplorerWidget::new();
        files.state().navigate(directory.path().to_path_buf());
        let id = |name: &str| crate::search_provider::entry_resource_id(&directory.path().join(name));
        let found = |files: &FileExplorerWidget, name: &str| {
            files
                .state()
                .entry_by_resource_id(&id(name))
                .map(|entry| entry.name.clone())
        };
        assert_eq!(found(&files, "b.txt").as_deref(), Some("b.txt"));
        assert_eq!(found(&files, "c.txt"), None);
        std::fs::write(directory.path().join("c.txt"), b"c").unwrap();
        std::fs::remove_file(directory.path().join("b.txt")).unwrap();
        files.state().refresh();
        assert_eq!(found(&files, "c.txt").as_deref(), Some("c.txt"));
        assert_eq!(found(&files, "b.txt"), None);
        assert_eq!(found(&files, "a.txt").as_deref(), Some("a.txt"));
    }

    phase an_unchanged_entry_publishes_the_row_it_published;
    {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.txt"), b"a").unwrap();
        std::fs::write(directory.path().join("b.txt"), b"b").unwrap();
        let files = FileExplorerWidget::new();
        files.state().navigate(directory.path().to_path_buf());
        let row = |files: &FileExplorerWidget, name: &str| {
            let id = crate::search_provider::entry_resource_id(&directory.path().join(name));
            files
                .state()
                .live_entry_rows()
                .into_iter()
                .find(|row| row.id() == id)
                .expect("the entry's row is published")
        };
        let a = row(&files, "a.txt");
        assert!(row(&files, "a.txt").same_row(&a), "the same row again");
        let b = row(&files, "b.txt");
        if let Some(entry) = files
            .state()
            .entries
            .iter_mut()
            .find(|entry| entry.name == "a.txt")
        {
            entry.size = entry.size.saturating_add(1);
        }
        assert!(
            !row(&files, "a.txt").same_row(&a),
            "a changed entry's row is new"
        );
        assert!(
            row(&files, "b.txt").same_row(&b),
            "an unchanged entry's row is kept"
        );
    }

    phase a_listing_read_that_finds_nothing_asks_for_no_sync;
    {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.txt"), b"a").unwrap();
        let mut files = FileExplorerWidget::new();
        files.state().navigate(directory.path().to_path_buf());
        files.sync(&automerge::AutoCommit::new(), &automerge::ROOT);
        let now = Instant::now();
        let long_ago = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
        files.state().directory_changed = true;
        files.state().last_directory_read = long_ago;
        assert!(
            !files.serve_deadline(now),
            "the folder is as it was: no sync"
        );
        std::fs::write(directory.path().join("b.txt"), b"b").unwrap();
        files.state().directory_changed = true;
        files.state().last_directory_read = long_ago;
        assert!(
            files.serve_deadline(now),
            "a new file asks for the sync that shows it"
        );
        assert!(
            files
                .state()
                .entries
                .iter()
                .any(|entry| entry.name == "b.txt")
        );
    }

    phase on_a_frame_loop_a_folder_shows_the_loading_state_until_its_worker_has_read_it;
    {
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("a.txt"), b"a").unwrap();
        on_a_frame_loop(|| {
            let files = hosted_window(home.path());
            settle_folder_read(&files);
            files.state().navigate(folder.clone());
            assert!(files.state().is_loading(), "the folder is being read");
            assert!(
                files.state().entries().is_empty(),
                "no rows of the folder left behind"
            );
            assert_eq!(files.state().current_path(), folder);
            assert_eq!(
                files.state().current_place(),
                None,
                "not known to be a folder yet"
            );
            settle_folder_read(&files);
            assert!(!files.state().is_loading());
            assert_eq!(
                files
                    .state()
                    .entries()
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>(),
                ["a.txt"]
            );
            assert!(
                files.state().current_place().is_some(),
                "a folder read is a place"
            );
        });
    }

    phase on_a_frame_loop_a_read_replaced_by_a_newer_navigation_is_never_shown;
    {
        let home = tempfile::tempdir().unwrap();
        let first = home.path().join("first");
        let second = home.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("old.txt"), b"").unwrap();
        std::fs::write(second.join("new.txt"), b"").unwrap();
        on_a_frame_loop(|| {
            let files = hosted_window(home.path());
            settle_folder_read(&files);
            files.state().navigate(first.clone());
            files.state().navigate(second.clone());
            settle_folder_read(&files);
            assert_eq!(files.state().current_path(), second);
            assert_eq!(
                files
                    .state()
                    .entries()
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>(),
                ["new.txt"]
            );
            assert!(
                !files.state().folder_reads.is_pending(),
                "the first read is dropped"
            );
        });
    }

    phase on_a_frame_loop_the_shown_folder_keeps_its_rows_while_it_is_read_again;
    {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("a.txt"), b"").unwrap();
        on_a_frame_loop(|| {
            let files = hosted_window(home.path());
            settle_folder_read(&files);
            let shown = files.state().entries().len();
            std::fs::write(home.path().join("b.txt"), b"").unwrap();
            files.state().refresh();
            assert!(
                !files.state().is_loading(),
                "a refresh does not blank the folder"
            );
            assert_eq!(
                files.state().entries().len(),
                shown,
                "its rows stay until replaced"
            );
            settle_folder_read(&files);
            assert_eq!(files.state().entries().len(), shown + 1);
        });
    }

    phase on_a_frame_loop_reveal_in_folder_selects_the_item_once_its_folder_is_read;
    {
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(folder.join(name), b"").unwrap();
        }
        on_a_frame_loop(|| {
            let files = hosted_window(home.path());
            settle_folder_read(&files);
            assert!(files.state().apply_reveal_in_folder(&folder.join("b.txt")));
            settle_folder_read(&files);
            let selected = {
                let state = files.state();
                state
                    .selected_index()
                    .map(|i| state.displayed_entries()[i].name.clone())
            };
            assert_eq!(selected.as_deref(), Some("b.txt"));
        });
    }
}

fn on_a_frame_loop(test: impl FnOnce() + Send) {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
                test();
            })
            .join()
            .unwrap();
    });
}

fn hosted_window(home: &Path) -> FileExplorerWidget {
    FileExplorerWidget::with_startup_inputs_and_services(
        StartupInputs {
            initial_dir: home.to_path_buf(),
            workspace_roots: Vec::new(),
            resource_roots: Vec::new(),
            remote_accounts: Vec::new(),
            trusted_humans: crate::identity_directory::TrustedHumanDirectory::default(),
        },
        HostedFileServices {
            thumbnails: None,
            local_clipboard: false,
        },
    )
}

fn settle_folder_read(files: &FileExplorerWidget) {
    tilo_test_env::wait_until("the folder read to finish", || {
        files.state().take_folder_read()
    });
}
