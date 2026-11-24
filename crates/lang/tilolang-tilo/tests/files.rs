//! Files end to end: the window over this device's folders, the verbs it runs on them, and what
//! it shows of the places a device publishes — a mounted workspace, attached drives and the
//! workspace's apps — through the actions and views the package itself publishes.
//!
//! The disk is a real temporary directory, so a rename, a paste and a trash are the file
//! operations the app asks for, and the view is read as a person sees it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tilolang::IdsLock;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::files::{DocumentApp, Entry, FileTarget, FilesHost, TrashEntry};
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const SOURCE: &str = include_str!("../../../../apps/tl/files/app.tl");

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The device's disk: a temporary home folder the app reads and changes by path, with a trash
/// that keeps what was put in it until it is restored or emptied.
#[derive(Clone)]
struct Disk {
    home: PathBuf,
    trash: Rc<RefCell<BTreeMap<String, String>>>,
}

impl Disk {
    fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            trash: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }

    /// Where `path` is on this disk: `~` is the person's home, as the app names it.
    fn resolve(&self, path: &str) -> PathBuf {
        if path == "~" {
            return self.home.clone();
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return self.home.join(rest);
        }
        PathBuf::from(path)
    }

    /// The path the app names this file by.
    fn app_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.home)
            .map(|inside| format!("~/{}", inside.display()))
            .unwrap_or_else(|_| path.display().to_string())
    }

    fn entry(&self, path: &Path, named: &str) -> Option<Entry> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        let name = path.file_name()?.to_string_lossy().into_owned();
        let modified = meta
            .modified()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| {
                i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
            });
        Some(Entry {
            hidden: name.starts_with('.'),
            name,
            path: named.to_owned(),
            folder: meta.is_dir(),
            size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
            modified,
        })
    }

    fn path_of(&self, parent: &str, name: &str) -> (PathBuf, String) {
        let named = if parent == "~" {
            format!("~/{name}")
        } else {
            format!("{}/{name}", parent.trim_end_matches('/'))
        };
        (self.resolve(&named), named)
    }
}

impl FilesHost for Disk {
    fn list(&self, path: &str) -> Result<Vec<Entry>, String> {
        let dir = self.resolve(path);
        let mut found = Vec::new();
        for item in std::fs::read_dir(&dir).map_err(|error| error.to_string())? {
            let item = item.map_err(|error| error.to_string())?;
            let name = item.file_name().to_string_lossy().into_owned();
            let named = if path == "~" {
                format!("~/{name}")
            } else {
                format!("{}/{name}", path.trim_end_matches('/'))
            };
            if let Some(entry) = self.entry(&item.path(), &named) {
                found.push(entry);
            }
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }

    fn stat(&self, path: &str) -> Result<Option<Entry>, String> {
        Ok(self.entry(&self.resolve(path), path))
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.resolve(path)).map_err(|error| error.to_string())
    }

    fn watch(&self, _path: &str, run: &str) -> Result<String, String> {
        Ok(format!("watch-{run}"))
    }

    fn mkdir(&self, path: &str) -> Result<Entry, String> {
        let dir = self.resolve(path);
        std::fs::create_dir(&dir).map_err(|error| error.to_string())?;
        self.entry(&dir, path)
            .ok_or_else(|| "the folder did not appear".to_owned())
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<Entry, String> {
        let file = self.resolve(path);
        std::fs::write(&file, bytes).map_err(|error| error.to_string())?;
        self.entry(&file, path)
            .ok_or_else(|| "the file did not appear".to_owned())
    }

    fn rename(&self, path: &str, name: &str) -> Result<Entry, String> {
        let from = self.resolve(path);
        let to = from
            .parent()
            .map(|parent| parent.join(name))
            .ok_or_else(|| "it has no folder".to_owned())?;
        std::fs::rename(&from, &to).map_err(|error| error.to_string())?;
        let named = self.app_path(&to);
        self.entry(&to, &named)
            .ok_or_else(|| "the entry did not appear".to_owned())
    }

    fn move_entry(&self, path: &str, folder: &str) -> Result<Entry, String> {
        let from = self.resolve(path);
        let name = from
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| "it has no name".to_owned())?;
        let (to, named) = self.path_of(folder, &name);
        std::fs::rename(&from, &to).map_err(|error| error.to_string())?;
        self.entry(&to, &named)
            .ok_or_else(|| "the entry did not appear".to_owned())
    }

    fn copy(&self, path: &str, folder: &str) -> Result<Entry, String> {
        let from = self.resolve(path);
        let name = from
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| "it has no name".to_owned())?;
        // A copy never replaces: a name the folder already holds is kept both, as the device's
        // own copy does.
        let mut landing = name.clone();
        let (mut to, mut named) = self.path_of(folder, &landing);
        let stem = Path::new(&name)
            .file_stem()
            .map_or(name.clone(), |stem| stem.to_string_lossy().into_owned());
        let extension = Path::new(&name)
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy()))
            .unwrap_or_default();
        let mut number = 2;
        while to.exists() {
            landing = format!("{stem} ({number}){extension}");
            (to, named) = self.path_of(folder, &landing);
            number += 1;
        }
        std::fs::copy(&from, &to).map_err(|error| error.to_string())?;
        self.entry(&to, &named)
            .ok_or_else(|| "the copy did not appear".to_owned())
    }

    fn remove(&self, path: &str) -> Result<bool, String> {
        let at = self.resolve(path);
        let was_there = at.exists();
        std::fs::remove_file(&at)
            .or_else(|_| std::fs::remove_dir_all(&at))
            .map_err(|error| error.to_string())?;
        Ok(was_there)
    }

    fn trash(&self, path: &str) -> Result<TrashEntry, String> {
        let from = self.resolve(path);
        let name = from
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| "it has no name".to_owned())?;
        let held = self.home.join(".Trash");
        std::fs::create_dir_all(&held).map_err(|error| error.to_string())?;
        let to = held.join(&name);
        std::fs::rename(&from, &to).map_err(|error| error.to_string())?;
        self.trash
            .borrow_mut()
            .insert(name.clone(), path.to_owned());
        Ok(TrashEntry {
            name,
            path: self.app_path(&to),
            original: path.to_owned(),
            deleted: 1_700_000_000,
        })
    }

    fn trash_entries(&self) -> Result<Vec<TrashEntry>, String> {
        let held = self.trash.borrow();
        let mut found: Vec<TrashEntry> = held
            .iter()
            .map(|(name, original)| TrashEntry {
                name: name.clone(),
                path: format!("~/.Trash/{name}"),
                original: original.clone(),
                deleted: 1_700_000_000,
            })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }

    fn restore(&self, name: &str) -> Result<Entry, String> {
        let original = self
            .trash
            .borrow_mut()
            .remove(name)
            .ok_or_else(|| "nothing in the trash has that name".to_owned())?;
        let to = self.resolve(&original);
        std::fs::rename(self.home.join(".Trash").join(name), &to)
            .map_err(|error| error.to_string())?;
        self.entry(&to, &original)
            .ok_or_else(|| "the entry did not come back".to_owned())
    }

    fn document_apps(&self, _path: &str) -> Result<Vec<DocumentApp>, String> {
        Ok(vec![DocumentApp {
            id: "so.tilo.word-processor".to_owned(),
            name: "Word Processor".to_owned(),
            is_default: true,
            icon: String::new(),
        }])
    }

    fn open(&self, _file: FileTarget<'_>) -> Result<(), String> {
        Ok(())
    }
}

/// The content store a dropped file's bytes are kept in.
#[derive(Clone, Default)]
struct Store {
    content: Rc<RefCell<BTreeMap<String, Vec<u8>>>>,
}

impl Store {
    fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
        self.keep(name, media, bytes).expect("the bytes are kept")
    }
}

impl BlobHost for Store {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().get(hash).cloned()
    }

    fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
        Ok(Vec::new())
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
        Err("this test writes no files out".to_owned())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let hash = format!("k{digest:x}");
        self.content
            .borrow_mut()
            .insert(hash.clone(), bytes.to_vec());
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }
}

/// What the app asked of an app that is not its own, such as the session's `open_app`.
#[derive(Clone, Default)]
struct Sent(Rc<RefCell<Vec<(String, Vec<Value>)>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        if let Invocation::Action { name, args, .. } = invocation {
            self.0.borrow_mut().push((
                name.clone(),
                args.iter().map(|(_, value)| value.clone()).collect(),
            ));
        }
        Invoked::Done(Value::Unit)
    }
}

/// Where a device publishes a reading of its own.
fn reading(kind: &str) -> ResourceRef {
    let ty = match kind {
        "drives" => "so.tilo.device#Drives",
        "apps" => "so.tilo.device#Apps",
        "folders" => "so.tilo.device#Folders",
        _ => panic!("no such device root"),
    };
    ResourceRef::parse_id(kind, ty).expect("a resource id")
}

struct Files {
    host: Host,
    root: ResourceRef,
    view: String,
    actions: Vec<(String, String)>,
    disk: Disk,
    store: Store,
    sent: Sent,
    _home: tempfile::TempDir,
}

impl Files {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("a home");
        let disk = Disk::new(home.path());
        let store = Store::default();
        let host = Host::new(me(), || None).expect("a host");
        let metas = tilolang_tilo::platform_apps::platform_packages()
            .expect("the platform's packages read");
        for meta in &metas {
            host.runtime()
                .load_foreign(meta.clone())
                .expect("the device is loaded");
        }
        let compiled = tilolang::compile_package(SOURCE, &IdsLock::new(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
        let package = host.load_compiled(&compiled).expect("the app loads");
        let view = package
            .meta
            .views
            .iter()
            .find(|view| view.name == "files")
            .expect("the window view")
            .id
            .clone();
        let actions = package
            .meta
            .actions
            .iter()
            .map(|action| (action.name.clone(), action.id.clone()))
            .collect();
        let root = host.runtime().create_root("Files").expect("the app's root");
        let sent = Sent::default();
        // The app's own actions run here, as its window's do; what it asks of the session is
        // sent, since the session is a package this host only reads.
        host.runtime().set_foreign_invoker(sent.clone());
        host.runtime().set_blobs(store.clone());
        host.runtime().set_device_files(disk.clone());
        let app = Self {
            host,
            root,
            view,
            actions,
            disk,
            store,
            sent,
            _home: home,
        };
        app.set_readings();
        app
    }

    /// What a device publishes of a mounted workspace, attached drives and the apps.
    fn set_readings(&self) {
        let folder = self.disk.home.join("Studio");
        self.host
            .runtime()
            .set_reading(
                &reading("folders"),
                &format!(
                    r#"{{"folders": [{{"workspace": "w1", "name": "Studio", "server": "",
                        "path": "{}", "state": "in-sync", "stuck": "", "refused": "",
                        "files": 2}}]}}"#,
                    folder.display()
                ),
            )
            .expect("the folders reading is set");
        self.host
            .runtime()
            .set_reading(
                &reading("drives"),
                r#"{"serving": true, "read_error": null, "drives": [
                    {"id": "drive-1", "label": "Camera card", "size_bytes": 32000000000,
                     "placement": "external", "hot_added": true, "remembered": null,
                     "needs_consent": false, "preselect": "open",
                     "can_remember": "nothing",
                     "mounted": {"path": "~/Media", "mode": "open", "read_only": null}}
                ], "remembered": [], "pending": [], "problem": null}"#,
            )
            .expect("the drives reading is set");
        self.host
            .runtime()
            .set_reading(
                &reading("apps"),
                r#"{"installed": [
                    {"key": "so.tilo.notes", "name": "Notes", "icon": "file-text",
                     "category": "work"},
                    {"key": "so.tilo.words", "name": "Writer", "icon": "pencil",
                     "category": "work"}
                ]}"#,
            )
            .expect("the apps reading is set");
    }

    fn call(&self, name: &str, args: Vec<Value>) -> Value {
        let id = self
            .actions
            .iter()
            .find(|(asked, _)| asked == name)
            .map(|(_, id)| id.clone())
            .unwrap_or_else(|| panic!("no action {name}"));
        match self
            .host
            .runtime()
            .invoke_action(&me(), (id.as_str(), args))
            .unwrap_or_else(|why| panic!("{name} runs: {why}"))
        {
            InvokeOutcome::Done(value) => value,
            other => panic!("{name} answers {other:?}"),
        }
    }

    fn window(&self) -> ViewInstance {
        self.host
            .runtime()
            .open_view(&me(), &self.view, vec![Value::resource(&self.root)])
            .expect("the window opens")
    }

    fn root_value(&self) -> Value {
        Value::resource(&self.root)
    }

    fn text(&self, path: &str) -> Value {
        Value::Text(path.to_owned())
    }
}

/// Every word a view shows: its texts, row titles, place labels and values, in order.
fn words(node: &UiNode, into: &mut Vec<String>) {
    for key in ["value", "title", "label"] {
        if let Some(Value::Text(text)) = node.props.get(key) {
            into.push(text.clone());
        }
    }
    for child in &node.children {
        words(child, into);
    }
}

fn shown(view: &mut ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    words(view.tree(), &mut out);
    // A list's rows are made as a window reads them: each row's own view draws its texts, and
    // the frame asks for every row it has.
    let lists: Vec<String> = view
        .tree()
        .of_kind("ui.VirtualList")
        .into_iter()
        .map(|list| list.key.clone())
        .collect();
    for list in lists {
        for index in 0..view.list_len(&list).unwrap_or(0) {
            if let Ok(Some(row)) = view.row(&list, index) {
                let mut row_words = Vec::new();
                words(row, &mut row_words);
                out.extend(row_words);
            }
        }
    }
    out
}

fn has(view: &mut ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text == wanted)
}

fn write(home: &Path, path: &str, body: &str) {
    let file = home.join(path);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).expect("the folder");
    }
    std::fs::write(file, body).expect("the file");
}

#[test]
fn the_window_shows_the_places_this_device_holds_and_the_folders_entries() {
    let app = Files::new();
    write(&app.disk.home, "Documents/plan.md", "# Plan");
    std::fs::create_dir_all(app.disk.home.join("Documents/Projects")).expect("a folder");

    app.call("arrive", vec![app.root_value()]);
    app.call("go", vec![app.root_value(), app.text("~/Documents")]);
    let mut window = app.window();
    let words = shown(&mut window);
    for wanted in ["Documents", "plan.md", "Projects"] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
    for wanted in [
        "Home",
        "Recent",
        "Applications",
        "Desktop",
        "Downloads",
        "Trash",
    ] {
        assert!(
            words.iter().any(|text| text == wanted),
            "the sidebar offers {wanted}: {words:?}"
        );
    }
}

#[test]
fn select_all_and_paste_copy_every_chosen_entry_and_a_taken_name_is_kept_both() {
    let app = Files::new();
    let home = app.disk.home.clone();
    write(&home, "Inbox/a.txt", "a");
    write(&home, "Inbox/b.txt", "b");
    std::fs::create_dir_all(home.join("Archive")).expect("a folder");
    write(&home, "Archive/a.txt", "older a");

    app.call("arrive", vec![app.root_value()]);
    app.call("go", vec![app.root_value(), app.text("~/Inbox")]);
    app.call("select_all", vec![app.root_value()]);
    app.call(
        "carry_selection",
        vec![app.root_value(), Value::Bool(false)],
    );
    app.call("go", vec![app.root_value(), app.text("~/Archive")]);
    app.call("paste", vec![app.root_value()]);

    assert_eq!(
        std::fs::read_to_string(home.join("Archive/a.txt")).expect("the older file"),
        "older a",
        "the file already there stays"
    );
    assert!(home.join("Archive/b.txt").exists(), "b is copied");
    assert!(
        home.join("Archive/a (2).txt").exists(),
        "the taken name is kept both: {:?}",
        std::fs::read_dir(home.join("Archive"))
            .expect("the folder")
            .map(|item| item.expect("an entry").file_name())
            .collect::<Vec<_>>()
    );
    assert!(
        home.join("Inbox/a.txt").exists(),
        "a copy leaves its source"
    );
}

#[test]
fn an_entry_cut_from_one_folder_moves_to_another_and_keeps_its_name() {
    let app = Files::new();
    let home = app.disk.home.clone();
    write(&home, "Downloads/report.pdf", "pdf");
    std::fs::create_dir_all(home.join("Documents")).expect("a folder");

    app.call("arrive", vec![app.root_value()]);
    app.call("go", vec![app.root_value(), app.text("~/Downloads")]);
    app.call(
        "choose",
        vec![
            app.root_value(),
            Value::List(vec![app.text("~/Downloads/report.pdf")]),
        ],
    );
    app.call("carry_selection", vec![app.root_value(), Value::Bool(true)]);
    app.call("go", vec![app.root_value(), app.text("~/Documents")]);
    app.call("paste", vec![app.root_value()]);

    assert!(home.join("Documents/report.pdf").exists(), "the file moved");
    assert!(
        !home.join("Downloads/report.pdf").exists(),
        "nothing is left behind"
    );
}

#[test]
fn a_file_dropped_on_the_window_lands_in_the_folder_shown() {
    let app = Files::new();
    let home = app.disk.home.clone();
    std::fs::create_dir_all(home.join("Documents")).expect("a folder");
    app.call("arrive", vec![app.root_value()]);
    app.call("go", vec![app.root_value(), app.text("~/Documents")]);

    let dropped = app.store.keeping("notes.md", "text/markdown", b"# Notes");
    app.call(
        "import_dropped",
        vec![app.root_value(), Value::List(vec![Value::file(dropped)])],
    );

    assert_eq!(
        std::fs::read_to_string(home.join("Documents/notes.md")).expect("the dropped file"),
        "# Notes"
    );
}

#[test]
fn a_mounted_workspace_a_drive_and_the_apps_are_places_the_sidebar_offers() {
    let app = Files::new();
    std::fs::create_dir_all(app.disk.home.join("Studio")).expect("the workspace's folder");
    std::fs::create_dir_all(app.disk.home.join("Media")).expect("the drive's folder");
    app.call("arrive", vec![app.root_value()]);
    let mut window = app.window();
    let words = shown(&mut window);
    for wanted in ["Studio", "Camera card", "Notes", "Writer"] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} stands in the window: {words:?}"
        );
    }

    // The workspace's mount and the open drive are places: choosing one shows what it holds.
    app.call("go", vec![app.root_value(), app.text("~/Media")]);
    assert!(has(&mut app.window(), "Media"));

    // The Applications place lists the workspace's apps, and pressing one opens it.
    app.call("go", vec![app.root_value(), app.text("applications")]);
    let mut window = app.window();
    assert!(has(&mut window, "Notes"), "the apps stand in the place");
    let row = window
        .tree()
        .of_kind("ui.ListItem")
        .into_iter()
        .find_map(|item| {
            item.of_kind("ui.ListRow")
                .into_iter()
                .find(|row| row.props.get("title") == Some(&Value::text("Notes")))
        })
        .expect("the Notes row");
    let fired = window
        .event_action(&row.key, "activate")
        .expect("the row opens its app");
    let ran = app
        .host
        .runtime()
        .invoke_action(&me(), (fired.action.as_str(), fired.args.clone()))
        .expect("opening the app runs");
    assert!(matches!(ran, InvokeOutcome::Done(_)), "{ran:?}");
    let opened = app.sent.0.borrow().clone();
    assert!(
        opened
            .iter()
            .any(|(name, args)| name == "open_app" && args == &vec![Value::text("so.tilo.notes")]),
        "the session opens Notes: {opened:?}"
    );
}
