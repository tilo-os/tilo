//! The standard library's file picker (`std.file_picker`) as an app embeds it: the app's actions
//! read the device's folders (`uses files`) and keep the picker's folder read as it changes, and
//! the picker gives back what the person chose. A pick answers the files chosen, a save asks
//! before it writes over a file already there, a cancel answers nothing, and a folder of ten
//! thousand entries opens building only the rows on screen.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use tilolang_runtime::files::{Entry, FilesHost};
    use tilolang_runtime::{
        AmStore, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
    };

    /// An app that asks for files the way the system's asking view does: it reads the folder the
    /// picker shows, watches it, and keeps what the person answered.
    const PICKING: &str = r#"app so.tilo.picking "Picking"

type Picking {
    mode: Text
    folder: Text
    entries: List<host.FileEntry>
    places: List<host.FileEntry>
    picked: List<Text>
    answered: Bool
}

root Picking

default view chooser for Picking

action open_folder(p: Picking, path: Text) uses files {
    match File.list(path) {
        .ok(entries) => {
            p.folder = path
            p.entries = entries
            match File.watch(path, "relist") {
                .ok(token) => { },
                .err(why) => { }
            }
        },
        .err(why) => { }
    }
}

action start(p: Picking, mode: Text) uses files {
    p.mode = mode
    p.picked = []
    p.answered = false
    match File.stat("/home/ada/Documents") {
        .ok(found) => { if let e = found { p.places = [e] } },
        .err(why) => { }
    }
    open_folder(p, "/home/ada")
}

action relist(p: Picking) uses files {
    match File.list(p.folder) {
        .ok(entries) => { p.entries = entries },
        .err(why) => { }
    }
}

action answer(p: Picking, paths: List<Text>) {
    p.picked = paths
    p.answered = true
}

action dismiss(p: Picking) {
    p.picked = []
    p.answered = true
}

view chooser(p: Picking) {
    std.file_picker(
        p.folder,
        p.entries,
        go: path -> open_folder(p, path),
        pick: paths -> answer(p, paths),
        cancel: dismiss(p),
        mode: if p.mode == "several" or p.mode == "notes" { "open" } else { p.mode },
        multiple: p.mode == "several",
        types: if p.mode == "notes" { [["Notes", ".md", ".txt"], ["All files"]] } else { [] },
        name: if p.mode == "save" { "plan.md" } else { "" },
        places: p.places,
        roots: ["/home/ada"],
        home: "/home/ada",
    )
}
"#;

    /// This device's disk: each path it holds, and whether it is a folder.
    #[derive(Clone, Default)]
    struct Disk {
        paths: Rc<RefCell<BTreeMap<String, bool>>>,
        watched: Rc<RefCell<Vec<(String, String)>>>,
    }

    impl Disk {
        fn folder(&self, path: &str) {
            self.paths.borrow_mut().insert(path.to_owned(), true);
        }

        fn file(&self, path: &str) {
            self.paths.borrow_mut().insert(path.to_owned(), false);
        }
    }

    impl FilesHost for Disk {
        fn list(&self, path: &str) -> Result<Vec<Entry>, String> {
            let paths = self.paths.borrow();
            if paths.get(path) != Some(&true) {
                return Err(format!("`{path}` is not a folder on this device"));
            }
            let prefix = format!("{path}/");
            Ok(paths
                .iter()
                .filter_map(|(at, folder)| {
                    let name = at.strip_prefix(&prefix)?;
                    (!name.contains('/')).then(|| Entry {
                        name: name.to_owned(),
                        path: at.clone(),
                        folder: *folder,
                        size: if *folder { 0 } else { 2048 },
                        modified: 1_790_000_000,
                        hidden: name.starts_with('.'),
                    })
                })
                .collect())
        }

        fn stat(&self, path: &str) -> Result<Option<Entry>, String> {
            let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
            Ok(self.list(folder)?.into_iter().find(|e| e.path == path))
        }

        fn read(&self, path: &str) -> Result<Vec<u8>, String> {
            Err(format!("`{path}` is not read here"))
        }

        fn watch(&self, path: &str, run: &str) -> Result<String, String> {
            self.watched
                .borrow_mut()
                .push((path.to_owned(), run.to_owned()));
            Ok(format!("watch:{path}"))
        }
    }

    struct Asking {
        rt: Runtime,
        app: App,
        p: ResourceRef,
        disk: Disk,
    }

    impl Asking {
        /// Ada's home: two folders, a note, a plan and a hidden file.
        fn new() -> Self {
            let disk = Disk::default();
            disk.folder("/home");
            disk.folder("/home/ada");
            disk.folder("/home/ada/Documents");
            disk.folder("/home/ada/Pictures");
            disk.file("/home/ada/notes.md");
            disk.file("/home/ada/plan.md");
            disk.file("/home/ada/photo.png");
            disk.file("/home/ada/.secrets");
            disk.file("/home/ada/Documents/report.txt");
            Self::on(disk)
        }

        fn on(disk: Disk) -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_device_files(disk.clone());
            let app = App::compile(PICKING, &[]);
            app.load(&rt);
            let p = rt.create_root("Picking").unwrap();
            Self { rt, app, p, disk }
        }

        fn call(&self, action: &str, mut args: Vec<Value>) {
            args.insert(0, Value::resource(&self.p));
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|e| panic!("{action}: {e:?}"));
        }

        /// The picker, asked as `mode` asks.
        fn ask(&self, mode: &str) -> ViewInstance {
            self.call("start", vec![Value::text(mode)]);
            let mut view = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("chooser"),
                    vec![Value::resource(&self.p)],
                )
                .unwrap();
            view.set_lazy_rows(false).unwrap();
            view
        }

        fn picked(&self) -> (bool, Vec<String>) {
            let answered = self.rt.read(&self.p, "answered").unwrap() == Value::Bool(true);
            let picked = match self.rt.read(&self.p, "picked").unwrap() {
                Value::List(items) => items
                    .iter()
                    .map(|v| match v {
                        Value::Text(t) => t.clone(),
                        other => panic!("a picked path is text, not {other:?}"),
                    })
                    .collect(),
                other => panic!("picked is a list, not {other:?}"),
            };
            (answered, picked)
        }
    }

    /// The titles of the picker's rows, in the order it lists them.
    fn rows(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.ListRow")
            .into_iter()
            .map(|row| text_prop(row, "title"))
            .collect()
    }

    fn row<'a>(view: &'a ViewInstance, title: &str) -> &'a UiNode {
        of_kind(view.tree(), "ui.ListRow")
            .into_iter()
            .find(|row| text_prop(row, "title") == title)
            .unwrap_or_else(|| panic!("no row {title} in\n{}", dump(view.tree())))
    }

    fn button<'a>(view: &'a ViewInstance, label: &str) -> &'a UiNode {
        of_kind(view.tree(), "ui.Button")
            .into_iter()
            .find(|b| matches!(b.props.get("label"), Some(Value::Text(l)) if l == label))
            .unwrap_or_else(|| panic!("no button {label} in\n{}", dump(view.tree())))
    }

    fn the_list(view: &ViewInstance) -> String {
        of_kind(view.tree(), "ui.VirtualList")
            .first()
            .unwrap_or_else(|| panic!("no list in\n{}", dump(view.tree())))
            .key
            .clone()
    }

    /// The person chooses `values` in the list, as a click or Ctrl-click does.
    fn choose(view: &mut ViewInstance, values: &[&str]) {
        let list = the_list(view);
        let chosen = Value::List(values.iter().map(|v| Value::text(*v)).collect());
        view.fire_with(&list, "selected_values", Some(&chosen))
            .unwrap();
        if let [one] = values {
            view.fire_with(&list, "select_value", Some(&Value::text(*one)))
                .unwrap();
        }
        view.poll_patches().unwrap();
    }

    fn press(view: &mut ViewInstance, label: &str) {
        let key = button(view, label).key.clone();
        view.fire(&key, "press").unwrap();
        view.poll_patches().unwrap();
    }

    fn activate(view: &mut ViewInstance, title: &str) {
        let key = row(view, title).key.clone();
        view.fire(&key, "activate").unwrap();
        view.poll_patches().unwrap();
    }

    fn enabled(view: &ViewInstance, label: &str) -> bool {
        on(button(view, label), "enabled", true)
    }

    /// A flag a control carries: a bool, or the int a bool's slot is drawn as.
    fn on(node: &UiNode, prop: &str, unset: bool) -> bool {
        match node.props.get(prop) {
            None => unset,
            Some(Value::Bool(b)) => *b,
            Some(Value::Int(i)) => *i != 0,
            Some(other) => panic!("{prop} is a flag, not {other:?}"),
        }
    }

    tilo_test_env::scenario! {
        fn a_pick_answers_what_the_person_chose_and_a_cancel_answers_nothing;

        phase the_folder_lists_folders_first_and_keeps_hidden_files_out;
        {
            let asking = Asking::new();
            let view = asking.ask("open");
            assert_eq!(
                rows(&view),
                ["Documents", "Pictures", "notes.md", "photo.png", "plan.md"],
                "folders first, then files by name, and no hidden file"
            );
            assert!(!enabled(&view, "Open"), "nothing is chosen yet, so nothing opens");
        }

        phase opening_a_folder_reads_it_and_the_breadcrumb_leads_back;
        {
            let asking = Asking::new();
            let mut view = asking.ask("open");
            activate(&mut view, "Documents");
            assert_eq!(rows(&view), ["report.txt"]);
            let crumb = of_kind(view.tree(), "ui.BreadcrumbSegment")
                .into_iter()
                .find(|s| text_prop(s, "label") == "Home")
                .unwrap_or_else(|| panic!("the path leads back home:\n{}", dump(view.tree())))
                .key
                .clone();
            view.fire(&crumb, "activate").unwrap();
            view.poll_patches().unwrap();
            assert_eq!(rows(&view).len(), 5, "back in ada's home");
            assert!(
                !of_kind(view.tree(), "ui.BreadcrumbSegment")
                    .iter()
                    .any(|s| text_prop(s, "label") == "home"),
                "the path stops at the root it may reach"
            );
        }

        phase a_chosen_file_is_what_the_pick_answers;
        {
            let asking = Asking::new();
            let mut view = asking.ask("open");
            choose(&mut view, &["/home/ada/plan.md"]);
            assert!(enabled(&view, "Open"));
            press(&mut view, "Open");
            assert_eq!(asking.picked(), (true, vec!["/home/ada/plan.md".to_owned()]));
        }

        phase activating_a_file_takes_it_at_once;
        {
            let asking = Asking::new();
            let mut view = asking.ask("open");
            activate(&mut view, "notes.md");
            assert_eq!(asking.picked(), (true, vec!["/home/ada/notes.md".to_owned()]));
        }

        phase several_files_come_back_together_without_the_folders_among_them;
        {
            let asking = Asking::new();
            let mut view = asking.ask("several");
            choose(
                &mut view,
                &["/home/ada/Pictures/", "/home/ada/notes.md", "/home/ada/photo.png"],
            );
            press(&mut view, "Open");
            assert_eq!(
                asking.picked(),
                (
                    true,
                    vec!["/home/ada/notes.md".to_owned(), "/home/ada/photo.png".to_owned()]
                )
            );
        }

        phase a_cancel_answers_nothing;
        {
            let asking = Asking::new();
            let mut view = asking.ask("open");
            choose(&mut view, &["/home/ada/plan.md"]);
            press(&mut view, "Cancel");
            assert_eq!(asking.picked(), (true, Vec::<String>::new()));
        }

        phase the_types_the_app_takes_narrow_the_files;
        {
            let asking = Asking::new();
            let mut view = asking.ask("notes");
            assert_eq!(rows(&view), ["Documents", "Pictures", "notes.md", "plan.md"]);
            let kind = of_kind(view.tree(), "ui.Dropdown")
                .into_iter()
                .find(|d| matches!(d.props.get("options"), Some(Value::List(o)) if o.contains(&Value::text("Notes"))))
                .unwrap_or_else(|| panic!("the types are offered:\n{}", dump(view.tree())))
                .key
                .clone();
            view.edit(&kind, &Value::text("All files")).unwrap();
            view.poll_patches().unwrap();
            assert_eq!(rows(&view).len(), 5, "every file, once the person asks for them all");
        }

        phase a_folder_pick_answers_the_folder_chosen_or_the_one_shown;
        {
            let asking = Asking::new();
            let mut view = asking.ask("folder");
            assert_eq!(rows(&view), ["Documents", "Pictures"], "a folder pick lists folders");
            choose(&mut view, &["/home/ada/Pictures/"]);
            press(&mut view, "Choose folder");
            assert_eq!(asking.picked(), (true, vec!["/home/ada/Pictures".to_owned()]));
            // Another ask, with nothing chosen: the folder shown is the answer.
            let asking = Asking::new();
            let mut view = asking.ask("folder");
            press(&mut view, "Choose folder");
            assert_eq!(asking.picked(), (true, vec!["/home/ada".to_owned()]));
        }
    }

    tilo_test_env::scenario! {
        fn a_save_asks_before_it_writes_over_a_file;

        phase a_new_name_saves_at_once;
        {
            let asking = Asking::new();
            let mut view = asking.ask("save");
            let field = of_kind(view.tree(), "ui.TextInput")[0].key.clone();
            view.edit(&field, &Value::text("draft.md")).unwrap();
            view.poll_patches().unwrap();
            press(&mut view, "Save");
            assert_eq!(asking.picked(), (true, vec!["/home/ada/draft.md".to_owned()]));
        }

        phase a_name_already_there_asks_first;
        {
            let asking = Asking::new();
            let mut view = asking.ask("save");
            // The name the app suggested is a file the folder holds.
            press(&mut view, "Save");
            assert_eq!(asking.picked(), (false, Vec::<String>::new()), "nothing is written yet");
            let dialog = of_kind(view.tree(), "ui.ModalDialog")[0];
            assert!(on(dialog, "open", false), "it asks");
            assert_eq!(text_prop(dialog, "title"), "Replace “plan.md”?");
            let dialog = dialog.key.clone();
            view.fire(&dialog, "act_secondary").unwrap();
            view.poll_patches().unwrap();
            assert_eq!(asking.picked(), (false, Vec::<String>::new()), "keeping it writes nothing");
            press(&mut view, "Save");
            view.fire(&dialog, "act_primary").unwrap();
            view.poll_patches().unwrap();
            assert_eq!(asking.picked(), (true, vec!["/home/ada/plan.md".to_owned()]));
        }

        phase choosing_a_file_names_the_save_after_it;
        {
            let asking = Asking::new();
            let mut view = asking.ask("save");
            choose(&mut view, &["/home/ada/notes.md"]);
            let field = of_kind(view.tree(), "ui.TextInput")[0].key.clone();
            assert_eq!(view.bound_value(&field), Some(Value::text("notes.md")));
        }
    }

    tilo_test_env::scenario! {
        fn the_picker_follows_the_folder_as_others_change_it;

        phase a_file_another_program_writes_shows_when_the_watch_runs;
        {
            let asking = Asking::new();
            let mut view = asking.ask("open");
            assert!(
                asking.disk.watched.borrow().iter().any(|(path, run)| path == "/home/ada" && run == "relist"),
                "the folder shown is watched"
            );
            asking.disk.file("/home/ada/arrived.pdf");
            asking.call("relist", vec![]);
            view.poll_patches().unwrap();
            assert!(rows(&view).contains(&"arrived.pdf".to_owned()));
        }
    }

    tilo_test_env::scenario! {
        fn ten_thousand_entries_open_building_only_the_rows_on_screen;

        phase the_folder_opens_and_lists_every_entry;
        {
            let disk = Disk::default();
            disk.folder("/home");
            disk.folder("/home/ada");
            for n in 0..10_000 {
                disk.file(&format!("/home/ada/a long name for the scan of page {n:05} of the archive.pdf"));
            }
            let asking = Asking::on(disk);
            asking.call("start", vec![Value::text("open")]);
            let started = std::time::Instant::now();
            let mut view = asking
                .rt
                .open_view(&me(), &asking.app.view("chooser"), vec![Value::resource(&asking.p)])
                .unwrap();
            let list = of_kind(view.tree(), "list")
                .into_iter()
                .chain(of_kind(view.tree(), "ui.VirtualList"))
                .find(|list| view.list_len(&list.key).is_some_and(|len| len > 1))
                .unwrap_or_else(|| panic!("the entries are a list:\n{}", dump(view.tree())))
                .key
                .clone();
            for row in 0..30 {
                view.row(&list, row).unwrap();
            }
            let took = started.elapsed();
            assert_eq!(view.list_len(&list), Some(10_000), "every entry is listed");
            let built = of_kind(view.tree(), "ui.ListRow").len();
            assert_eq!(built, 30, "only the rows on screen are built");
            assert!(
                took < std::time::Duration::from_secs(5),
                "ten thousand entries open with a screen of rows in {took:?}"
            );
        }
    }
}
