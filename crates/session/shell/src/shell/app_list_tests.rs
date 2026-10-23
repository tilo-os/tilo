use super::*;
use crate::shell::uninstall_sheet::{Answer, Asking, REMOVE_DATA};
use desktop_entry::EntryKind;
use std::path::Path;

fn places(root: &Path) -> Places {
    Places {
        data_home: root.join(".local/share"),
    }
}

fn tilo(id: &str, name: &str, in_user_store: bool, in_image: bool) -> TiloApp {
    TiloApp {
        package_id: id.into(),
        name: name.into(),
        in_user_store,
        in_image,
    }
}

fn host(id: &str, name: &str, path: PathBuf) -> HostEntry {
    HostEntry {
        desktop_id: id.into(),
        name: name.into(),
        path,
        flatpak: None,
        downloaded: None,
        kind: EntryKind::System,
        downloaded_locations: Vec::new(),
    }
}

/// One of every kind the person named: a Tilo app they added, one the
/// system image ships, a Flatpak of theirs and one of the system's, an
/// `AppImage`, a menu entry of the system's and one of their own.
fn every_kind(root: &Path) -> Vec<AppListItem> {
    every_kind_for(root, &Person::default())
}

fn every_kind_for(root: &Path, person: &Person) -> Vec<AppListItem> {
    let places = places(root);
    let apps = places.applications();
    let tilo_apps = [
        tilo("so.example.journal", "Journal", true, false),
        tilo("so.tilo.notes", "Notes", false, true),
    ];
    let mut chess = host(
        "org.gnome.Chess",
        "Chess",
        places
            .data_home
            .join("flatpak/exports/share/applications/org.gnome.Chess.desktop"),
    );
    chess.flatpak = Some("org.gnome.Chess".into());
    chess.kind = EntryKind::Flatpak;
    let mut maps = host(
        "org.gnome.Maps",
        "Maps",
        PathBuf::from("/var/lib/flatpak/exports/share/applications/org.gnome.Maps.desktop"),
    );
    maps.flatpak = Some("org.gnome.Maps".into());
    maps.kind = EntryKind::Flatpak;
    let mut appimage = host(
        "tilo-downloaded-krita",
        "Krita",
        apps.join("tilo-downloaded/krita.desktop"),
    );
    appimage.downloaded = Some(root.join("Applications/krita-5.2.AppImage"));
    appimage.kind = EntryKind::AppImage;
    let mut script = host("my-script", "My script", apps.join("my-script.desktop"));
    script.kind = EntryKind::Program;
    let host_entries = [
        chess,
        maps,
        appimage,
        host(
            "org.gnome.TextEditor",
            "Text Editor",
            PathBuf::from("/usr/share/applications/org.gnome.TextEditor.desktop"),
        ),
        script,
        // Answered by the installed Tilo package of the same id.
        host(
            "so.tilo.notes",
            "Notes (desktop entry)",
            PathBuf::from("/usr/share/applications/so.tilo.notes.desktop"),
        ),
    ];
    project(&tilo_apps, &host_entries, &places, person)
}

fn item<'a>(items: &'a [AppListItem], id: &str) -> &'a AppListItem {
    items
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("{id} is listed: {items:#?}"))
}

/// Every kind of app is in the one list, each once.
#[test]
fn every_kind_of_app_is_in_the_list() {
    let root = Path::new("/home/ute");
    let items = every_kind(root);
    assert_eq!(
        item(&items, "so.example.journal").source,
        AppSource::Tilo { added: true }
    );
    assert_eq!(
        item(&items, "so.tilo.notes").source,
        AppSource::Tilo { added: false }
    );
    assert_eq!(
        item(&items, "org.gnome.Chess").source,
        AppSource::Flatpak {
            app_id: "org.gnome.Chess".into(),
            installation: FlatpakInstallation::Person
        }
    );
    assert!(matches!(
        item(&items, "org.gnome.Maps").source,
        AppSource::Flatpak {
            installation: FlatpakInstallation::Device {
                administrator: false
            },
            ..
        }
    ));
    assert!(matches!(
        &item(&items, "tilo-downloaded-krita").source,
        AppSource::Downloaded { program, .. } if program.ends_with("krita-5.2.AppImage")
    ));
    assert_eq!(
        item(&items, "org.gnome.TextEditor").source,
        AppSource::System
    );
    assert!(matches!(
        item(&items, "my-script").source,
        AppSource::PersonEntry { .. }
    ));
    assert_eq!(
        items
            .iter()
            .filter(|item| item.id == "so.tilo.notes")
            .count(),
        1,
        "a desktop entry a Tilo package answers for is the package's: {items:#?}"
    );
    let names: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_by_key(|name| name.to_lowercase());
    assert_eq!(names, sorted, "listed by name");
}

/// What uninstalling each kind does, as Files and the sheet are told.
#[test]
fn each_kind_says_how_it_leaves() {
    let items = every_kind(Path::new("/home/ute"));
    let removal = |id| item(&items, id).removal();
    assert_eq!(removal("org.gnome.Chess"), AppRemoval::UninstallWithData);
    assert_eq!(removal("so.example.journal"), AppRemoval::Uninstall);
    assert_eq!(removal("tilo-downloaded-krita"), AppRemoval::Uninstall);
    assert_eq!(removal("org.gnome.TextEditor"), AppRemoval::RemoveFromApps);
    assert_eq!(removal("org.gnome.Maps"), AppRemoval::RemoveFromApps);
    assert_eq!(removal("my-script"), AppRemoval::RemoveFromApps);
    assert_eq!(removal("so.tilo.notes"), AppRemoval::RemoveFromApps);
    let entry = item(&items, "tilo-downloaded-krita").entry();
    assert_eq!(
        entry.program.as_deref(),
        Some("/home/ute/Applications/krita-5.2.AppImage")
    );
    assert_eq!(
        item(&items, "so.example.journal")
            .entry()
            .package
            .as_deref(),
        Some("so.example.journal")
    );
}

fn switch_in(dialog: &ui::widget::ModalDialog<ui::widget::Column>) -> bool {
    fn walk(widget: &dyn ui::widget::Widget) -> bool {
        widget.test_id() == Some(REMOVE_DATA) || widget.children().into_iter().any(walk)
    }
    walk(dialog.content())
}

fn words_in(sheet: &mut Asking) -> Vec<String> {
    let dialog = std::mem::replace(
        &mut sheet.dialog,
        ui::widget::ModalDialog::new("", ui::widget::Column::new()),
    );
    let mut framed = ui::headless::Framed::new(
        ui::State::new(),
        dialog,
        (1280, 800),
        ui::theme::Theme::dark(),
    );
    let _ = ui::headless::Framed::harness_mut(&mut framed).poll_frame();
    ui::headless::Framed::harness(&framed)
        .a11y_tree_update()
        .nodes
        .iter()
        .flat_map(|(_, node)| {
            [
                node.label().map(str::to_owned),
                // A row's lines under its title are its value.
                (node.role() == ui::accesskit::Role::ListItem)
                    .then(|| node.value().map(str::to_owned))
                    .flatten(),
                (node.role() == ui::accesskit::Role::Switch)
                    .then(|| node.description().map(|it| format!("switch: {it}")))
                    .flatten(),
            ]
        })
        .flatten()
        .collect()
}

#[test]
fn a_device_flatpak_says_it_goes_for_everyone_and_the_data_is_only_yours() {
    let administrator = Person {
        administrator: true,
        removed_from_apps: Vec::new(),
    };
    let home = Path::new("/home/ute");
    let mut device =
        Asking::new(item(&every_kind_for(home, &administrator), "org.gnome.Maps").clone());
    let words = words_in(&mut device);
    assert!(
        words
            .iter()
            .any(|it| it == "It's removed for everyone on this computer."),
        "{words:?}"
    );
    assert!(
        words
            .iter()
            .any(|it| it
                == "switch: Your settings and saved files go to the Trash, where you can restore them."),
        "{words:?}"
    );
    let mut own = Asking::new(item(&every_kind(home), "org.gnome.Chess").clone());
    assert!(!words_in(&mut own).iter().any(|it| it.contains("everyone")));
}

/// Uninstalling asks first, and the "remove data" switch is on by default for a
/// Flatpak and absent for every other kind.
#[test]
fn the_data_switch_is_on_for_a_flatpak_and_absent_otherwise() {
    let items = every_kind(Path::new("/home/ute"));
    let flatpak = Asking::new(item(&items, "org.gnome.Chess").clone());
    assert!(flatpak.offers_data_switch());
    assert!(switch_in(&flatpak.dialog), "the sheet holds the switch");
    assert!(flatpak.remove_data.get(), "on by default");
    assert_eq!(
        flatpak.answer(),
        None,
        "nothing happens before the person answers"
    );
    for id in [
        "so.example.journal",
        "tilo-downloaded-krita",
        "org.gnome.TextEditor",
    ] {
        let sheet = Asking::new(item(&items, id).clone());
        assert!(!sheet.offers_data_switch(), "{id}");
        assert!(!switch_in(&sheet.dialog), "{id} has no data switch");
    }
}

/// The switch's answer reaches the uninstall; turning it off keeps the data.
#[test]
fn the_answer_carries_the_switch() {
    let items = every_kind(Path::new("/home/ute"));
    let chess = item(&items, "org.gnome.Chess").clone();
    let mut sheet = Asking::new(chess.clone());
    sheet.remove_data.set(false);
    // The person presses Uninstall on the sheet as shown.
    let dialog = std::mem::replace(
        &mut sheet.dialog,
        ui::widget::ModalDialog::new("", ui::widget::Column::new()),
    );
    let mut framed = ui::headless::Framed::new(
        ui::State::new(),
        dialog,
        (1280, 800),
        ui::theme::Theme::dark(),
    );
    let _ = ui::headless::Framed::harness_mut(&mut framed).poll_frame();
    let button = framed.primary_rect().center();
    ui::headless::Framed::harness_mut(&mut framed).click_at(button.x, button.y);
    assert_eq!(
        sheet.answer(),
        Some(Answer::Uninstall {
            item: chess,
            remove_data: false
        })
    );
}

/// A Flatpak's data goes to the Trash, from where it can be restored, never
/// deleted; Flatpak itself is never told to delete it.
#[test]
fn flatpak_data_goes_to_the_trash() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", home);
    env.set("XDG_DATA_HOME", home.join(".local/share"));
    let items = every_kind(home);
    let chess = item(&items, "org.gnome.Chess");
    let data = flatpak_data_dir(home, "org.gnome.Chess");
    std::fs::create_dir_all(data.join("config")).unwrap();
    std::fs::write(data.join("config/games.json"), b"{\"won\": 3}").unwrap();

    let (steps, outcome) = plan(chess, true, &places(home), home).unwrap();
    assert_eq!(outcome, Outcome::UninstalledDataInTrash);
    assert!(
        steps.iter().all(|step| match step {
            Step::Flatpak { args, .. } => !args.iter().any(|arg| arg == "--delete-data"),
            _ => true,
        }),
        "{steps:?}"
    );
    let trash_steps: Vec<&Step> = steps
        .iter()
        .filter(|step| matches!(step, Step::Trash(_)))
        .collect();
    assert_eq!(trash_steps, [&Step::Trash(data.clone())]);
    // Carried out: the data is in the Trash, whole.
    assert_eq!(execute(trash_steps[0]), Ok(true));
    assert!(!data.exists());
    let trashed = home.join(".local/share/Trash/files/org.gnome.Chess/config/games.json");
    assert_eq!(std::fs::read(&trashed).unwrap(), b"{\"won\": 3}");

    let (kept, outcome) = plan(chess, false, &places(home), home).unwrap();
    assert_eq!(outcome, Outcome::Uninstalled);
    assert!(
        !kept.iter().any(|step| matches!(step, Step::Trash(_))),
        "{kept:?}"
    );
}

/// Uninstalling a Tilo app removes the app and nothing else: its documents are
/// the person's workspace data and are never touched.
#[test]
fn tilo_app_data_is_untouched() {
    let home = Path::new("/home/ute");
    let items = every_kind(home);
    let (steps, outcome) = plan(
        item(&items, "so.example.journal"),
        true,
        &places(home),
        home,
    )
    .unwrap();
    assert_eq!(outcome, Outcome::Uninstalled);
    assert_eq!(
        steps,
        [Step::App(vec![
            "remove".into(),
            "--".into(),
            "so.example.journal".into()
        ])],
        "only the app goes: `app remove` keeps its saved state, and nothing is trashed"
    );
}

#[test]
fn a_tilo_app_of_the_system_image_is_hidden_and_then_not_listed() {
    let home = Path::new("/home/ute");
    let (steps, outcome) = plan(
        item(&every_kind(home), "so.tilo.notes"),
        true,
        &places(home),
        home,
    )
    .unwrap();
    assert_eq!(outcome, Outcome::RemovedFromApps);
    assert_eq!(
        steps,
        [Step::Hide {
            at: places(home).applications().join("so.tilo.notes.desktop"),
            name: "Notes".into(),
        }]
    );
    let person = Person {
        administrator: false,
        removed_from_apps: vec!["so.tilo.notes".into()],
    };
    let items = every_kind_for(home, &person);
    assert!(
        items.iter().all(|item| item.id != "so.tilo.notes"),
        "{items:#?}"
    );
    assert!(items.iter().any(|item| item.id == "so.example.journal"));
}

#[test]
fn a_device_flatpak_is_uninstalled_by_an_administrator_and_hidden_for_anyone_else() {
    let home = Path::new("/home/ute");
    let administrator = Person {
        administrator: true,
        removed_from_apps: Vec::new(),
    };
    let maps = item(&every_kind_for(home, &administrator), "org.gnome.Maps").clone();
    assert_eq!(maps.removal(), AppRemoval::UninstallWithData);
    let (steps, outcome) = plan(&maps, true, &places(home), home).unwrap();
    assert_eq!(outcome, Outcome::UninstalledDataInTrash);
    assert_eq!(
        steps,
        [
            Step::Flatpak {
                args: [
                    "uninstall",
                    "--system",
                    "--noninteractive",
                    "--",
                    "org.gnome.Maps"
                ]
                .map(str::to_owned)
                .to_vec(),
                required: true,
            },
            Step::Flatpak {
                args: ["uninstall", "--system", "--unused", "--noninteractive"]
                    .map(str::to_owned)
                    .to_vec(),
                required: false,
            },
            Step::Trash(flatpak_data_dir(home, "org.gnome.Maps")),
        ]
    );

    let maps = item(&every_kind(home), "org.gnome.Maps").clone();
    assert_eq!(maps.removal(), AppRemoval::RemoveFromApps);
    let (steps, outcome) = plan(&maps, true, &places(home), home).unwrap();
    assert_eq!(outcome, Outcome::RemovedFromApps);
    assert!(matches!(steps.as_slice(), [Step::Hide { .. }]), "{steps:?}");
}

/// A menu entry of the operating system offers no Uninstall, only "Remove from
/// apps", which hides it and deletes nothing.
#[test]
fn an_os_entry_offers_only_remove_from_apps() {
    let home = Path::new("/home/ute");
    let items = every_kind(home);
    let xwayland = item(&items, "org.gnome.TextEditor");
    assert_eq!(xwayland.removal(), AppRemoval::RemoveFromApps);
    let sheet = Asking::new(xwayland.clone());
    assert!(!sheet.offers_data_switch());
    assert_eq!(sheet.action, "Remove from apps");
    assert_eq!(sheet.title, "Remove Text Editor from apps?");
    let (steps, outcome) = plan(xwayland, true, &places(home), home).unwrap();
    assert_eq!(outcome, Outcome::RemovedFromApps);
    assert_eq!(
        steps,
        [Step::Hide {
            at: home.join(".local/share/applications/org.gnome.TextEditor.desktop"),
            name: "Text Editor".into()
        }]
    );
    assert!(hidden_entry("Text Editor").contains("\nHidden=true\n"));
    // The person's own entry file goes to the Trash rather than being hidden
    // under a second file.
    let (steps, _) = plan(item(&items, "my-script"), true, &places(home), home).unwrap();
    assert_eq!(
        steps,
        [Step::Trash(
            home.join(".local/share/applications/my-script.desktop")
        )]
    );
}

/// An uninstalled `AppImage` goes to the Trash and its entry goes; nothing
/// outside the person's applications directory is ever forgotten.
#[test]
fn a_downloaded_program_goes_to_the_trash_and_its_entry_goes() {
    let home = Path::new("/home/ute");
    let items = every_kind(home);
    let krita = item(&items, "tilo-downloaded-krita");
    let sheet = Asking::new(krita.clone());
    assert_eq!(sheet.title, "Uninstall Krita?");
    assert_eq!(sheet.action, "Uninstall");
    let (steps, outcome) = plan(krita, true, &places(home), home).unwrap();
    assert_eq!(outcome, Outcome::UninstalledFileInTrash);
    assert_eq!(
        steps,
        [
            Step::Trash(home.join("Applications/krita-5.2.AppImage")),
            Step::ForgetEntry {
                data_home: home.join(".local/share"),
                desktop_id: "tilo-downloaded-krita".to_owned(),
            },
        ]
    );
    let mut stray = krita.clone();
    stray.source = AppSource::Downloaded {
        program: home.join("a.AppImage"),
        entry: PathBuf::from("/etc/passwd"),
        appimage: true,
        locations: Vec::new(),
    };
    assert!(plan(&stray, true, &places(home), home).is_err());
}

/// An id that could leave the applications directory is refused before
/// anything is written.
#[test]
fn a_hostile_id_is_never_hidden_outside_the_applications_directory() {
    let home = Path::new("/home/ute");
    for id in ["../../.bashrc", "a/b", ".hidden", ""] {
        let item = AppListItem {
            id: id.into(),
            name: "x".into(),
            icon: None,
            size: None,
            source: AppSource::System,
        };
        assert!(plan(&item, true, &places(home), home).is_err(), "{id}");
    }
}

/// An entry no menu shows is in no list: an Xwayland-style `NoDisplay`
/// entry, a `Hidden` one, another desktop's and one without `Exec` leave
/// the app list as they leave search and the launcher, which read the same
/// scan.
#[test]
fn entries_no_menu_shows_are_not_apps() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("applications");
    std::fs::create_dir_all(&dir).unwrap();
    let write = |id: &str, extra: &str| {
        std::fs::write(
            dir.join(format!("{id}.desktop")),
            format!("[Desktop Entry]\nType=Application\nName={id}\n{extra}"),
        )
        .unwrap();
    };
    write(
        "org.freedesktop.Xwayland",
        "Exec=Xwayland\nNoDisplay=true\nTerminal=false\nCategories=System;\n",
    );
    write("hidden", "Exec=hidden\nHidden=true\n");
    write("gnome-only", "Exec=gnome-only\nOnlyShowIn=GNOME;\n");
    write("not-here", "Exec=not-here\nNotShowIn=Tilo;\n");
    write("no-exec", "");
    write("org.gnome.TextEditor", "Exec=gnome-text-editor\n");
    let scan = desktop_entry::scan_dirs(&[dir], &["Tilo".to_owned()]);
    let items = project(
        &[],
        &host_entries(&scan),
        &places(root.path()),
        &Person::default(),
    );
    let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["org.gnome.TextEditor"]);
}

/// A Flatpak's size is measured from its installation's files, found from
/// the entry the installation exported.
#[test]
fn a_flatpaks_files_are_found_beside_its_exported_entry() {
    assert_eq!(
        flatpak_files(
            Path::new(
                "/home/ute/.local/share/flatpak/exports/share/applications/org.gnome.Chess.desktop"
            ),
            "org.gnome.Chess",
        ),
        Some(PathBuf::from(
            "/home/ute/.local/share/flatpak/app/org.gnome.Chess/current/active/files"
        ))
    );
    assert_eq!(
        flatpak_files(Path::new("/usr/share/applications/x.desktop"), "x"),
        None
    );
}

/// Taking a downloaded program off the person's apps removes its entry and
/// the icon Tilo kept for it; the program's file stays where it is.
#[test]
fn a_downloaded_program_taken_off_the_apps_is_no_longer_listed_and_its_file_stays() {
    use desktop_entry::downloaded::{Icon, Identity, Program, ProgramKind, Store};
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let places = places(home);
    let program = home.join("Downloads/mytool/mytool");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::write(&program, b"\x7fELF").unwrap();
    let store = Store::in_data_home(&places.data_home);
    let desktop_id = store
        .add(
            &Identity {
                key: "org.example.MyTool".into(),
                name: "MyTool".into(),
                icon: Icon::sniff(b"\x89PNG\r\n\x1a\n".to_vec()),
                comment: None,
                categories: Vec::new(),
                keywords: Vec::new(),
                mime_types: Vec::new(),
                startup_wm_class: None,
                field_code: None,
            },
            &Program {
                path: program.clone(),
                kind: ProgramKind::Program,
            },
        )
        .unwrap();
    let listed = || {
        let scan = desktop_entry::scan_dirs(&[places.applications()], &[]);
        project(&[], &host_entries(&scan), &places, &Person::default())
    };
    let items = listed();
    let mytool = item(&items, &desktop_id).clone();
    assert!(matches!(
        &mytool.source,
        AppSource::Downloaded { program: listed, .. } if *listed == program
    ));
    let icon = desktop_entry::scan_dirs(&[places.applications()], &[])
        .find(&desktop_id)
        .and_then(|entry| entry.icon.clone())
        .map(PathBuf::from)
        .expect("the entry keeps an icon");
    assert!(icon.exists());

    assert!(!execute(&forget_entry(&mytool, &places)).unwrap());
    assert!(
        listed().iter().all(|item| item.id != desktop_id),
        "the entry is gone from the list"
    );
    assert!(program.exists(), "the program's file stays");
    assert!(
        !icon.exists(),
        "the icon Tilo kept for it goes with the entry"
    );
}

/// Uninstalling a downloaded program sends every copy of it the person has
/// to the Trash, so a leftover one cannot bring the app back, and the
/// question says how many and where.
#[test]
fn uninstalling_a_downloaded_program_trashes_every_copy_and_says_where() {
    let home = Path::new("/home/ute");
    let items = every_kind(home);
    let mut krita = item(&items, "tilo-downloaded-krita").clone();
    let AppSource::Downloaded { locations, .. } = &mut krita.source else {
        panic!("krita is a downloaded program");
    };
    locations.push(home.join("Downloads/krita-5.1/bin/krita"));
    let (steps, outcome) = plan(&krita, true, &places(home), home).unwrap();
    assert_eq!(outcome, Outcome::UninstalledFilesInTrash);
    assert_eq!(
        steps,
        [
            Step::Trash(home.join("Applications/krita-5.2.AppImage")),
            Step::Trash(home.join("Downloads/krita-5.1/bin/krita")),
            Step::ForgetEntry {
                data_home: home.join(".local/share"),
                desktop_id: "tilo-downloaded-krita".to_owned(),
            },
        ]
    );
    let mut sheet = Asking::new(krita);
    let rows = copy_rows(&sheet.dialog);
    assert_eq!(rows, ["app-uninstall.copy.0", "app-uninstall.copy.1"]);
    let words = words_in(&mut sheet);
    // Each copy by name, with the folder it is in.
    for expected in [
        "All 2 copies go to the Trash, where you can restore them.",
        "krita-5.2.AppImage",
        "krita",
    ] {
        assert!(
            words.iter().any(|word| word == expected),
            "{expected}: {words:?}"
        );
    }
    for folder in ["Applications", "Downloads/krita-5.1/bin"] {
        assert!(
            words.iter().any(|word| word.ends_with(folder)),
            "{folder}: {words:?}"
        );
    }
}

fn copy_rows(dialog: &ui::widget::ModalDialog<ui::widget::Column>) -> Vec<String> {
    fn walk(widget: &dyn ui::widget::Widget, out: &mut Vec<String>) {
        if let Some(id) = widget.test_id()
            && id.starts_with(crate::shell::uninstall_sheet::COPY)
        {
            out.push(id.to_owned());
        }
        for child in widget.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(dialog.content(), &mut out);
    out
}
