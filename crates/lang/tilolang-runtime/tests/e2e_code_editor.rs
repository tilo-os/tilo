//! The Code Editor (`apps/tl/code-editor/app.tl`) end to end: a project's files made, moved,
//! written and saved through its actions; a folder read from the device and a file written
//! back; a command run and a project searched through the one engine the app calls; a language
//! server opened, told and asked as the app's held connection, its word read as it arrives; and
//! its window, with the tree, the code and the file's state.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use std::sync::{Arc, Mutex};
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::host_work::{EngineHost, HeldHost, JobHandlers, Push, held_slot};
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value};

    const SOURCE: &str = include_str!("../../../../apps/tl/code-editor/app.tl");

    /// The device's engine: what it was asked, and the answers it gives.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        /// Whether the next save finds the file changed on disk.
        conflict: Rc<RefCell<bool>>,
        /// The actions each started job named as it moves and ends.
        handlers: Rc<RefCell<Vec<JobHandlers>>>,
    }

    impl Engine {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }

        fn asks(&self, op: &str) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .filter(|(asked, _)| asked == op)
                .map(|(_, input)| input.clone())
                .collect()
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                "list" if input.contains("/nowhere") => Err("no folder stands there".to_owned()),
                "list" if input.contains("/p/src") => Ok(r#"{"entries":[
                    {"name":"main.rs","path":"/p/src/main.rs","folder":false,"hidden":false}
                ]}"#
                .to_owned()),
                "list" => Ok(r#"{"entries":[
                    {"name":"src","path":"/p/src","folder":true,"hidden":false},
                    {"name":"README.md","path":"/p/README.md","folder":false,"hidden":false},
                    {"name":"main.go","path":"/p/main.go","folder":false,"hidden":false},
                    {"name":".git","path":"/p/.git","folder":true,"hidden":true}
                ]}"#
                .to_owned()),
                "load" if input.contains("README") => {
                    Ok(r##"{"text":"# demo\n","mtime":11,"len":8}"##.to_owned())
                }
                "load" if input.contains("main.rs") => {
                    Ok(r#"{"text":"fn main() {}\n","mtime":12,"len":13}"#.to_owned())
                }
                "load" => Ok(r#"{"text":"","mtime":1,"len":0}"#.to_owned()),
                "save" if *self.conflict.borrow() && !input.contains("\"force\":true") => {
                    Err("conflict".to_owned())
                }
                "save" => Ok(r#"{"mtime":42,"len":17}"#.to_owned()),
                "stat" => Ok(r#"{"exists":true,"mtime":42,"len":17}"#.to_owned()),
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            handlers: &JobHandlers,
        ) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            let job = match op {
                "run" => "run-1",
                "search" => "search-1",
                other => return Err(format!("`{other}` is no job this device starts")),
            };
            self.handlers.borrow_mut().push(handlers.clone());
            Ok(job.to_owned())
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            let said = || -> Result<String, String> {
                match job {
                "run-1" => Ok(
                    r#"{"state":"done","exit":101,"output":"src/main.rs:3:5: error: expected `;`\ntest tests::it ... ok\n","truncated":false,"failure":""}"#
                        .to_owned(),
                ),
                "search-1" => Ok(
                    r#"{"state":"done","hits":[{"path":"/p/src/main.rs","rel":"src/main.rs","line":0,"column":9,"len":4,"preview":"fn main() {}"}],"files":2,"truncated":false}"#
                        .to_owned(),
                ),
                other => Err(format!("no job `{other}` runs on this device")),
            }
            };
            Ok(said()
                .ok()
                .and_then(|said| tilolang_tilo::engine::record_of(job, &said)))
        }

        fn cancel(&self, job: &str) -> bool {
            job == "run-1" || job == "search-1"
        }
    }

    /// The device's held connections: what the app opened, sent and closed, and a way to have a
    /// connection say something, as the app's connection process pushes it.
    #[derive(Clone, Default)]
    struct Servers {
        opened: Arc<Mutex<Vec<(String, String, String)>>>,
        sent: Arc<Mutex<Vec<(String, String)>>>,
        closed: Arc<Mutex<Vec<String>>>,
        push: Arc<Mutex<Option<Arc<dyn Push>>>>,
        /// Why the next open is refused, when it is.
        refuse: Arc<Mutex<Option<String>>>,
    }

    impl Servers {
        /// Connection `key` says `word` of itself, as its process pushes it.
        fn say(&self, key: &str, word: &str) {
            let push = self
                .push
                .lock()
                .unwrap()
                .clone()
                .expect("the runtime follows");
            push.state(&held_slot(key), word);
        }
    }

    impl HeldHost for Servers {
        fn open(&self, app: &str, kind: &str, key: &str, ask: &str) -> Result<String, String> {
            if app != "so.tilo.code-editor" {
                return Err(format!("{app} opened the code editor's connection"));
            }
            let refused = self.refuse.lock().unwrap().take();
            if let Some(why) = refused {
                return Err(why);
            }
            // This device has no Go language server, as the code engine says of one it cannot find.
            if ask.contains("\"Go\"") {
                return Err("gopls is not installed".to_owned());
            }
            self.opened
                .lock()
                .unwrap()
                .push((kind.to_owned(), key.to_owned(), ask.to_owned()));
            Ok("{}".to_owned())
        }

        fn send(&self, key: &str, message: &str) -> Result<String, String> {
            self.sent
                .lock()
                .unwrap()
                .push((key.to_owned(), message.to_owned()));
            Ok("{}".to_owned())
        }

        fn close(&self, key: &str) -> bool {
            self.closed.lock().unwrap().push(key.to_owned());
            true
        }

        fn follow(&self, push: Arc<dyn Push>) {
            *self.push.lock().unwrap() = Some(push);
        }
    }

    /// The device's content store: what a file a person chose holds.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = format!("h{}", bytes.len());
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    struct Editor {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        engine: Engine,
        servers: Servers,
        device: Device,
    }

    impl Editor {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let engine = Engine::default();
            rt.set_engine(engine.clone());
            let servers = Servers::default();
            rt.set_held(servers.clone());
            let device = Device::default();
            rt.set_blobs(device.clone());
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let root = rt.create_root("Project").unwrap();
            Self {
                rt,
                app,
                root,
                engine,
                servers,
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// Drives the job `job` as the device does: runs the `done:` action its start named,
        /// handed the record the device keeps for it.
        fn settles(&self, job: &str) {
            let record = self
                .engine
                .job(job)
                .unwrap()
                .unwrap_or_else(|| panic!("the device keeps `{job}`"));
            let kept = self
                .engine
                .handlers
                .borrow()
                .last()
                .cloned()
                .expect("a job was started");
            let handler = kept.done.expect("the start named its end");
            let call = self.rt.job_call(&handler, &record).unwrap();
            self.rt
                .invoke_action(&me(), call)
                .unwrap_or_else(|error| panic!("`{job}`'s handler: {error:?}"));
        }

        /// The call as it comes out, refusal and all.
        fn attempt(&self, action: &str, args: Vec<Value>) -> bool {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).is_ok()
        }

        fn file(&self, name: &str, folder: Option<&ResourceRef>) -> ResourceRef {
            let parent = match folder {
                Some(folder) => Value::some(Value::resource(folder)),
                None => Value::none(),
            };
            returned_ref(self.call(
                "new_file",
                vec![
                    Value::resource(&self.root),
                    Value::text(name),
                    parent,
                    Value::text(""),
                ],
            ))
        }

        fn folder(&self, name: &str, parent: Option<&ResourceRef>) -> ResourceRef {
            let held = match parent {
                Some(parent) => Value::some(Value::resource(parent)),
                None => Value::none(),
            };
            returned_ref(self.call(
                "new_folder",
                vec![Value::resource(&self.root), Value::text(name), held],
            ))
        }

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.rt.read(r, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "a document's whole numbers are far inside i64"
        )]
        fn number(&self, r: &ResourceRef, field: &str) -> i64 {
            match self.rt.read(r, field).unwrap() {
                Value::Int(n) => n,
                Value::Float(n) if n.is_finite() => n as i64,
                Value::Float(_) => 0,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// The number a field holds, kept as a float: a size, which an `Int` would lose.
        fn float(&self, r: &ResourceRef, field: &str) -> f64 {
            match self.rt.read(r, field).unwrap() {
                Value::Float(n) => n,
                Value::Int(n) => f64::from(i32::try_from(n).unwrap_or(0)),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(b) => b,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn value(&self, r: &ResourceRef, field: &str) -> Value {
            self.rt.read(r, field).unwrap()
        }

        /// The id a declaration of the compiled app takes, for comparing an enum case.
        fn id(&self, name: &str) -> String {
            self.app
                .compiled
                .ids
                .get(name)
                .map_or_else(|| panic!("no id {name}"), ToString::to_string)
        }

        /// The values of a `List` field: a list of records is a value list, not references.
        fn list(&self, r: &ResourceRef, field: &str) -> Vec<Value> {
            match self.value(r, field) {
                Value::List(items) => items,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// One field of a record value, as a `List<T>` field holds them.
        fn rec(&self, value: &Value, field: &str) -> Value {
            match value {
                Value::Rec(record) => record
                    .fields
                    .get(&self.id(field))
                    .unwrap_or_else(|| panic!("no {field} in {record:?}"))
                    .clone(),
                other => panic!("not a record: {other:?}"),
            }
        }

        fn opt_ref(&self, r: &ResourceRef, field: &str) -> Option<ResourceRef> {
            match self.value(r, field) {
                Value::Opt(Some(held)) => match *held {
                    Value::Ref(found) => Some(Rc::unwrap_or_clone(found.target)),
                    other => panic!("{field} holds {other:?}"),
                },
                Value::Opt(None) => None,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn records(&self, r: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(r, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn window(&self) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("project"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap()
        }
    }

    #[test]
    fn a_project_starts_with_a_file_in_the_folder_it_names() {
        let editor = Editor::new();
        let src = editor.folder("src", None);
        let main = editor.file("main.rs", Some(&src));
        assert_eq!(editor.text(&main, "name"), "main.rs");
        assert_eq!(editor.text(&main, "language"), "Rust");
        assert_eq!(editor.text(&main, "text"), "");
        match editor.value(&main, "kind") {
            Value::Enum(kind) => {
                assert_eq!(
                    kind.variant,
                    editor.id("EntryKind.file"),
                    "a file is a file"
                );
            }
            other => panic!("kind reads {other:?}"),
        }

        let listed = editor.records(&editor.root, "files");
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn a_new_file_is_an_entry_with_its_language() {
        let editor = Editor::new();
        let readme = editor.file("README.md", None);
        assert_eq!(editor.text(&readme, "language"), "Markdown");
        let script = editor.file("run.sh", None);
        assert_eq!(editor.text(&script, "language"), "Shell");

        editor.call(
            "rename",
            vec![
                Value::resource(&editor.root),
                Value::resource(&readme),
                Value::text("guide.py"),
            ],
        );
        assert_eq!(editor.text(&readme, "name"), "guide.py");
        assert_eq!(
            editor.text(&readme, "language"),
            "Python",
            "a name says the language"
        );

        // Two entries of one name in one folder are refused.
        let _other = editor.file("same.rs", None);
        assert!(
            !editor.attempt(
                "new_file",
                vec![
                    Value::resource(&editor.root),
                    Value::text("same.rs"),
                    Value::none(),
                    Value::text(""),
                ],
            ),
            "a name is taken once in a folder"
        );
    }

    #[test]
    fn a_folder_holds_entries_and_never_holds_itself() {
        let editor = Editor::new();
        let src = editor.folder("src", None);
        let ui = editor.folder("ui", Some(&src));
        let main = editor.file("main.rs", Some(&src));
        assert_eq!(
            editor.opt_ref(&main, "parent").map(|p| p.id_string()),
            Some(src.id_string()),
            "the file stands in the folder"
        );

        editor.call(
            "move_entry",
            vec![
                Value::resource(&editor.root),
                Value::resource(&src),
                Value::some(Value::resource(&ui)),
                Value::none(),
            ],
        );
        // A folder cannot move into its own child.
        assert!(
            !editor.attempt(
                "move_entry",
                vec![
                    Value::resource(&editor.root),
                    Value::resource(&ui),
                    Value::some(Value::resource(&src)),
                    Value::none(),
                ],
            ),
            "a folder cannot hold itself"
        );
    }

    #[test]
    fn deleting_a_folder_takes_its_entries() {
        let editor = Editor::new();
        let src = editor.folder("src", None);
        let _main = editor.file("main.rs", Some(&src));
        let _lib = editor.file("lib.rs", Some(&src));
        let _readme = editor.file("README.md", None);
        editor.call(
            "delete",
            vec![Value::resource(&editor.root), Value::resource(&src)],
        );
        let left = editor.records(&editor.root, "files");
        assert_eq!(left.len(), 1, "the folder and its files went");
        assert_eq!(editor.text(&left[0], "name"), "README.md");
    }

    #[test]
    fn a_text_edit_splices_the_file() {
        let editor = Editor::new();
        let main = editor.file("main.rs", None);
        editor.call(
            "replace_text",
            vec![Value::resource(&main), Value::text("fn main() {}")],
        );
        editor.call(
            "replace_range",
            vec![
                Value::resource(&main),
                Value::Int(3),
                Value::Int(4),
                Value::text("other"),
            ],
        );
        assert_eq!(editor.text(&main, "text"), "fn other() {}");
    }

    #[test]
    fn line_operations_duplicate_move_delete_and_comment() {
        let editor = Editor::new();
        let main = editor.file("main.rs", None);
        editor.call(
            "replace_text",
            vec![Value::resource(&main), Value::text("one\ntwo\nthree")],
        );
        editor.call(
            "duplicate_line",
            vec![Value::resource(&main), Value::Int(0)],
        );
        assert_eq!(editor.text(&main, "text"), "one\none\ntwo\nthree");

        editor.call(
            "move_line",
            vec![Value::resource(&main), Value::Int(3), Value::Bool(true)],
        );
        assert_eq!(editor.text(&main, "text"), "one\none\nthree\ntwo");

        editor.call(
            "toggle_comment",
            vec![Value::resource(&main), Value::Int(2)],
        );
        assert_eq!(editor.text(&main, "text"), "one\none\n// three\ntwo");
        editor.call(
            "toggle_comment",
            vec![Value::resource(&main), Value::Int(2)],
        );
        assert_eq!(editor.text(&main, "text"), "one\none\nthree\ntwo");

        editor.call("indent_line", vec![Value::resource(&main), Value::Int(0)]);
        assert_eq!(editor.text(&main, "text"), "    one\none\nthree\ntwo");
        editor.call("outdent_line", vec![Value::resource(&main), Value::Int(0)]);
        assert_eq!(editor.text(&main, "text"), "one\none\nthree\ntwo");

        editor.call("delete_line", vec![Value::resource(&main), Value::Int(3)]);
        assert_eq!(editor.text(&main, "text"), "one\none\nthree");
    }

    #[test]
    fn finding_and_replacing_in_a_file() {
        let editor = Editor::new();
        let main = editor.file("main.rs", None);
        editor.call(
            "replace_text",
            vec![
                Value::resource(&main),
                Value::text("let offset = 1;\nlet fs = 2;\nlet refs = offset;"),
            ],
        );
        let found = editor.call(
            "find_text",
            vec![
                Value::resource(&main),
                Value::text("fs"),
                Value::Bool(false),
                Value::Bool(true),
            ],
        );
        match found {
            InvokeOutcome::Done(Value::List(items)) => {
                assert_eq!(items.len(), 1, "only the word `fs`, not `offset` or `refs`");
            }
            other => panic!("the find answered {other:?}"),
        }
        editor.call(
            "replace_in_file",
            vec![
                Value::resource(&main),
                Value::text("let "),
                Value::text("const "),
                Value::Bool(false),
                Value::Bool(false),
            ],
        );
        assert!(editor.text(&main, "text").contains("const offset"));
        assert!(editor.text(&main, "text").contains("const fs"));
    }

    #[test]
    fn the_declarations_of_a_file_are_its_symbols() {
        let editor = Editor::new();
        let main = editor.file("main.rs", None);
        editor.call(
            "replace_text",
            vec![
                Value::resource(&main),
                Value::text("pub fn crab() {}\nlet x = 1;\nstruct Shell {}\n"),
            ],
        );
        let symbols = editor.call("symbols", vec![Value::resource(&main)]);
        match symbols {
            InvokeOutcome::Done(Value::List(items)) => {
                assert_eq!(items.len(), 2, "the function and the struct");
            }
            other => panic!("the symbols answered {other:?}"),
        }
    }

    #[test]
    fn every_language_the_editor_knows_reads_by_its_extension() {
        let editor = Editor::new();
        let cases = [
            ("a.rs", "Rust"),
            ("a.toml", "TOML"),
            ("a.md", "Markdown"),
            ("a.json", "JSON"),
            ("a.yml", "YAML"),
            ("a.py", "Python"),
            ("a.tsx", "TypeScript"),
            ("a.css", "CSS"),
            ("a.sv", "SystemVerilog"),
            ("a.unknown", "Plain Text"),
        ];
        for (name, language) in cases {
            let file = editor.file(name, None);
            assert_eq!(editor.text(&file, "language"), language, "{name}");
        }
    }

    #[test]
    fn the_window_colours_a_file_as_the_language_it_is_written_in() {
        let cases = [
            ("a.rs", "rust"),
            ("a.toml", "toml"),
            ("a.md", "markdown"),
            ("a.json", "json"),
            ("a.yml", "yaml"),
            ("a.py", "python"),
            ("a.js", "javascript"),
            ("a.ts", "typescript"),
            ("a.html", "html"),
            ("a.css", "css"),
            ("a.sh", "shell"),
            ("a.c", "c"),
            ("a.cpp", "cpp"),
            ("a.go", "go"),
            ("a.v", "verilog"),
            ("a.sv", "system_verilog"),
            ("a.unknown", "plain_text"),
        ];
        for (name, language) in cases {
            let editor = Editor::new();
            editor.file(name, None);
            let window = editor.window();
            let node = of_kind(window.tree(), "ui.CodeEditor")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("no editor for {name}"));
            assert_eq!(text_prop(node, "language"), language, "{name}");
        }
    }

    #[test]
    fn a_file_is_kept_as_the_language_a_person_chose() {
        let editor = Editor::new();
        let plain = editor.file("notes", None);
        assert_eq!(editor.text(&plain, "language"), "Plain Text");
        editor.call(
            "set_language",
            vec![Value::resource(&plain), Value::text("Python")],
        );
        assert_eq!(editor.text(&plain, "language"), "Python");
        assert!(
            !editor.attempt(
                "set_language",
                vec![Value::resource(&plain), Value::text("Cobol")],
            ),
            "a language the editor does not know is refused"
        );
    }

    #[test]
    fn opening_a_folder_reads_its_files_through_the_engine() {
        let editor = Editor::new();
        editor.call(
            "open_folder",
            vec![Value::resource(&editor.root), Value::text("/p")],
        );
        assert_eq!(editor.text(&editor.root, "folder"), "/p");
        let files = editor.records(&editor.root, "files");
        assert!(
            files.iter().any(|f| editor.text(f, "name") == "README.md"),
            "the folder's file is an entry; trouble: {:?}; files: {:?}; ops: {:?}",
            editor.text(&editor.root, "trouble"),
            files
                .iter()
                .map(|f| editor.text(f, "name"))
                .collect::<Vec<_>>(),
            editor.engine.ops()
        );
        let readme = files
            .iter()
            .find(|f| editor.text(f, "name") == "README.md")
            .expect("the readme");
        assert_eq!(editor.text(readme, "text"), "# demo\n");
        assert_eq!(editor.number(readme, "stamp"), 11);
        assert!(
            !files.iter().any(|f| editor.text(f, "name") == ".git"),
            "a hidden folder is not a source file"
        );
        assert!(
            editor
                .engine
                .asks("list")
                .iter()
                .any(|ask| ask.contains("/p")),
            "the device read the folder"
        );
    }

    #[test]
    fn saving_checks_the_file_did_not_change_under_it() {
        let editor = Editor::new();
        editor.call(
            "open_folder",
            vec![Value::resource(&editor.root), Value::text("/p")],
        );
        let readme = editor
            .records(&editor.root, "files")
            .into_iter()
            .find(|f| editor.text(f, "name") == "README.md")
            .expect("the readme");

        editor.call(
            "replace_text",
            vec![Value::resource(&readme), Value::text("# changed\n")],
        );
        editor.call(
            "save",
            vec![Value::resource(&editor.root), Value::resource(&readme)],
        );
        assert_eq!(editor.text(&readme, "saved"), "# changed\n");
        assert_eq!(editor.number(&readme, "stamp"), 42);
        let saved = editor.engine.asks("save");
        assert!(
            saved.iter().any(|ask| ask.contains("\"mtime\":11")),
            "the save checks the stamp it read: {saved:?}"
        );

        // A file something else changed is not written over without the person saying so.
        *editor.engine.conflict.borrow_mut() = true;
        editor.call(
            "replace_text",
            vec![Value::resource(&readme), Value::text("# second\n")],
        );
        editor.call(
            "save",
            vec![Value::resource(&editor.root), Value::resource(&readme)],
        );
        assert!(editor.flag(&readme, "stale"), "the file changed on disk");
        assert_eq!(
            editor.text(&readme, "saved"),
            "# changed\n",
            "nothing was written"
        );

        editor.call(
            "overwrite",
            vec![Value::resource(&editor.root), Value::resource(&readme)],
        );
        assert!(!editor.flag(&readme, "stale"));
        assert_eq!(editor.text(&readme, "saved"), "# second\n");
    }

    #[test]
    fn a_command_runs_and_its_output_names_the_places() {
        let editor = Editor::new();
        editor.call(
            "run",
            vec![
                Value::resource(&editor.root),
                Value::text("cargo build"),
                Value::text("/p"),
            ],
        );
        let commands = editor.records(&editor.root, "commands");
        assert_eq!(commands.len(), 1);
        assert_eq!(editor.text(&commands[0], "state"), "running");

        editor.settles("run-1");
        assert_eq!(editor.text(&commands[0], "state"), "done");
        assert_eq!(editor.number(&commands[0], "exit"), 101);
        assert!(editor.text(&commands[0], "output").contains("expected"));
        let items = editor.list(&commands[0], "items");
        assert!(
            items
                .iter()
                .any(|i| editor.rec(i, "Item.path") == Value::text("src/main.rs")
                    && editor.rec(i, "Item.line") == Value::Int(3)),
            "the compiler's place is an item: {items:?}"
        );

        editor.call(
            "rerun",
            vec![Value::resource(&editor.root), Value::resource(&commands[0])],
        );
        assert_eq!(editor.records(&editor.root, "commands").len(), 2);
        assert_eq!(editor.engine.asks("run").len(), 2);
    }

    #[test]
    fn a_project_search_runs_and_its_result_opens_the_file() {
        let editor = Editor::new();
        let _notes = editor.file("notes.md", None);
        editor.call(
            "search_project",
            vec![
                Value::resource(&editor.root),
                Value::text("main"),
                Value::Bool(false),
                Value::Bool(false),
            ],
        );
        editor.settles("search-1");
        assert_eq!(
            editor.number(&editor.root, "search_files"),
            2,
            "state {:?} job {:?} trouble {:?} hits {:?}",
            editor.text(&editor.root, "search_state"),
            editor.text(&editor.root, "search_job"),
            editor.text(&editor.root, "trouble"),
            editor.list(&editor.root, "search_hits").len()
        );
        let hits = editor.list(&editor.root, "search_hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(editor.rec(&hits[0], "Hit.rel"), Value::text("src/main.rs"));

        // The result's own control opens the place, as a person presses it: the action runs as
        // the window, so it can set the window's state.
        let mut window = editor.window();
        let results = of_kind(window.tree(), "ui.MenuItem")
            .into_iter()
            .find(|item| item.props.get("label") == Some(&Value::text("Search results")))
            .map(|item| item.key.clone())
            .expect("the panel offers the search results");
        window
            .fire(&results, "choose")
            .expect("the panel shows the results");
        window.poll_patches().expect("the window draws the results");
        let row = of_kind(window.tree(), "ui.ListRow")
            .into_iter()
            .find(|node| {
                node.props
                    .get("meta")
                    .and_then(Value::as_text)
                    .is_some_and(|meta| meta.contains("src/main.rs"))
            })
            .map_or_else(
                || {
                    panic!(
                        "the result's row is in the panel: {}",
                        crate::common::dump(window.tree())
                    )
                },
                |node| node.key.clone(),
            );
        window.fire(&row, "activate").expect("the result opens");
        window.poll_patches().expect("the window draws the file");
        let opened = editor
            .app
            .meta()
            .views
            .iter()
            .find(|view| view.name == "project")
            .and_then(|view| view.state.iter().find(|state| state.name == "opened"))
            .expect("the project declares `opened`");
        let shown = window
            .view_state(&opened.id)
            .expect("the window shows a file");
        let main_id = match shown {
            Value::Opt(Some(held)) => match *held {
                Value::Ref(found) => found.target.id_string(),
                other => panic!("opened reads {other:?}"),
            },
            other => panic!("opened reads {other:?}"),
        };
        let files = editor.records(&editor.root, "files");
        let main = files
            .iter()
            .find(|f| editor.text(f, "name") == "main.rs")
            .expect("the result opened its file");
        assert_eq!(editor.text(main, "text"), "fn main() {}\n");
        assert_eq!(main.id_string(), main_id, "the window shows the hit's file");
        assert_eq!(editor.number(main, "reveal_line"), 0);
    }

    #[test]
    fn the_window_search_finds_in_the_open_file_and_across_the_project() {
        let editor = Editor::new();
        let main = editor.file("main.rs", None);
        editor.call(
            "replace_text",
            vec![
                Value::resource(&main),
                Value::text("fn other() {}\nlet held = main();\nlet both = main();\n"),
            ],
        );
        let _notes = editor.file("notes.md", None);
        let mut window = editor.window();
        let shell = of_kind(window.tree(), "ui.NavShell")[0].key.clone();
        window
            .fire_with(&shell, "search", Some(&Value::text("main")))
            .unwrap();
        window.poll_patches().unwrap();

        let found = editor.list(&editor.root, "found");
        assert_eq!(
            found.len(),
            2,
            "both lines of the file hold the words: {found:?}"
        );
        let code = of_kind(window.tree(), "ui.CodeEditor")
            .into_iter()
            .next()
            .expect("the open file is drawn");
        assert_eq!(
            code.props.get("reveal"),
            Some(&Value::text("2:12:1")),
            "the first place the words stand is shown: {code:?}"
        );
        assert!(
            editor
                .engine
                .asks("search")
                .iter()
                .any(|ask| ask.contains("\"needle\":\"main\"")),
            "the project is searched for the same words: {:?}",
            editor.engine.asks("search")
        );

        // The words the project search was asked find the file's own places replaced.
        window
            .fire_with(&shell, "search", Some(&Value::text("")))
            .unwrap();
        window.poll_patches().unwrap();
        assert!(
            editor.list(&editor.root, "found").is_empty(),
            "clearing the search takes the places away"
        );
    }

    #[test]
    fn a_language_server_is_the_projects_held_connection() {
        let editor = Editor::new();
        editor.call(
            "open_folder",
            vec![Value::resource(&editor.root), Value::text("/p")],
        );
        let rust = editor
            .records(&editor.root, "files")
            .into_iter()
            .find(|f| editor.text(f, "name") == "main.rs")
            .expect("the file the device listed");
        // Checking a Rust file opens the project's language server for Rust and that folder, as
        // a held connection, never an engine job, and tells it what the file holds.
        editor.call(
            "check",
            vec![Value::resource(&editor.root), Value::resource(&rust)],
        );
        let opened = editor.servers.opened.lock().unwrap().clone();
        assert_eq!(opened.len(), 1, "one server opened: {opened:?}");
        let (kind, key, ask) = &opened[0];
        assert_eq!(kind, "lsp");
        assert_eq!(key, "Rust:/p");
        assert!(ask.contains("\"root\":\"/p\""), "{ask}");
        assert_eq!(editor.text(&editor.root, "server"), "Rust:/p");
        assert!(
            !editor.engine.ops().iter().any(|op| op == "server"),
            "the engine starts no server: {:?}",
            editor.engine.ops()
        );
        let sent = editor.servers.sent.lock().unwrap().clone();
        assert!(
            sent.iter()
                .any(|(key, told)| key == "Rust:/p" && told.contains("fn main() {}")),
            "the server was told the file: {sent:?}"
        );

        // What the server says arrives as its word, pushed, and an action reads the newest.
        editor.servers.say(
            "Rust:/p",
            r#"{"state":"ready","server":"rust-analyzer","failure":"","progress":null,"diagnostics":{"/p/src/main.rs":[{"severity":"warning","line":0,"column":3,"message":"unused import"}]},"answer":""}"#,
        );
        editor.call(
            "check",
            vec![Value::resource(&editor.root), Value::resource(&rust)],
        );
        assert_eq!(editor.text(&rust, "source"), "rust-analyzer");
        assert_eq!(
            editor.servers.opened.lock().unwrap().len(),
            2,
            "a check joins the connection this device holds"
        );

        editor.call(
            "complete",
            vec![
                Value::resource(&editor.root),
                Value::resource(&rust),
                Value::Int(0),
                Value::Int(0),
            ],
        );
        assert!(editor.servers.sent.lock().unwrap().iter().any(|(_, ask)| {
            ask.contains("\"what\":\"complete\"") && ask.contains("\"op\":\"ask\"")
        }));

        // Trying again ends the connection; the next check opens it anew.
        editor.call("restart_server", vec![Value::resource(&editor.root)]);
        assert_eq!(
            editor.servers.closed.lock().unwrap().clone(),
            vec!["Rust:/p".to_owned()]
        );
        assert_eq!(editor.text(&editor.root, "server"), "");
    }

    #[test]
    fn a_server_that_cannot_start_says_why() {
        let editor = Editor::new();
        editor.call(
            "open_folder",
            vec![Value::resource(&editor.root), Value::text("/p")],
        );
        let file = editor
            .records(&editor.root, "files")
            .into_iter()
            .find(|f| editor.text(f, "name") == "main.rs")
            .expect("the file the device listed");
        *editor.servers.refuse.lock().unwrap() = Some("rust-analyzer is not installed".to_owned());
        editor.call(
            "check",
            vec![Value::resource(&editor.root), Value::resource(&file)],
        );
        assert_eq!(editor.text(&editor.root, "server"), "");
        assert_eq!(
            editor.text(&editor.root, "server_trouble"),
            "The Rust language server is not installed on this computer. Install it with \
             “rustup component add rust-analyzer”, then try again."
        );
        // The editor's own check stands instead.
        assert_eq!(editor.text(&file, "source"), "lint");
    }

    #[test]
    fn a_language_servers_progress_reads_through() {
        let editor = Editor::new();
        editor.call(
            "open_folder",
            vec![Value::resource(&editor.root), Value::text("/p")],
        );
        let rust = editor
            .records(&editor.root, "files")
            .into_iter()
            .find(|f| editor.text(f, "name") == "main.rs")
            .expect("the file the device listed");
        editor.call(
            "check",
            vec![Value::resource(&editor.root), Value::resource(&rust)],
        );
        let shown = |word: &str| {
            editor.servers.say("Rust:/p", word);
            format!("{:?}", editor.window().tree())
        };
        let busy = shown(
            r#"{"state":"busy","server":"rust-analyzer","failure":"","progress":{"title":"Indexing","detail":"crates","percent":42},"diagnostics":{},"answer":""}"#,
        );
        assert!(
            busy.contains("Indexing 42%"),
            "the progress the server says reads into the status line: {busy}"
        );
        // Work the server reports without a percentage shows its title alone.
        let untold = shown(
            r#"{"state":"busy","server":"rust-analyzer","failure":"","progress":{"title":"Indexing","detail":"","percent":null},"diagnostics":{},"answer":""}"#,
        );
        assert!(
            untold.contains("Indexing") && !untold.contains("Indexing 0%"),
            "{untold}"
        );
        let idle = shown(
            r#"{"state":"ready","server":"rust-analyzer","failure":"","progress":null,"diagnostics":{},"answer":""}"#,
        );
        assert!(
            !idle.contains("Indexing"),
            "a server doing nothing shows no progress: {idle}"
        );
    }

    #[test]
    fn the_editors_own_checks_stand_until_a_server_answers() {
        let editor = Editor::new();
        let broken = editor.file("broken.rs", None);
        editor.call(
            "replace_text",
            vec![
                Value::resource(&broken),
                Value::text("fn main() { \n    let x = 1;\n"),
            ],
        );
        editor.call(
            "check",
            vec![Value::resource(&editor.root), Value::resource(&broken)],
        );
        let problems = editor.list(&broken, "problems");
        assert!(
            !problems.is_empty(),
            "an open bracket is a problem: {problems:?}"
        );
        assert_eq!(editor.text(&broken, "source"), "lint");
        assert!(!editor.text(&broken, "checked").is_empty());
    }

    #[test]
    fn a_server_that_is_missing_says_how_to_install_it() {
        let editor = Editor::new();
        editor.call(
            "open_folder",
            vec![Value::resource(&editor.root), Value::text("/p")],
        );
        let go = editor
            .records(&editor.root, "files")
            .into_iter()
            .find(|f| editor.text(f, "name") == "main.go")
            .expect("the Go file the device listed");
        editor.call(
            "check",
            vec![Value::resource(&editor.root), Value::resource(&go)],
        );
        let trouble = editor.text(&editor.root, "server_trouble");
        assert!(trouble.contains("not installed"), "{trouble}");
        assert!(
            trouble.contains("gopls"),
            "the way to install it: {trouble}"
        );
        assert_eq!(editor.text(&editor.root, "server"), "", "no server is held");
    }

    #[test]
    fn language_servers_can_be_turned_off() {
        let editor = Editor::new();
        assert!(editor.flag(&editor.root, "language_servers"));
        editor.call(
            "toggle_language_servers",
            vec![Value::resource(&editor.root)],
        );
        assert!(!editor.flag(&editor.root, "language_servers"));
        let file = editor.file("main.rs", None);
        editor.call(
            "check",
            vec![Value::resource(&editor.root), Value::resource(&file)],
        );
        assert_eq!(editor.text(&editor.root, "server"), "", "no server started");
        assert!(editor.servers.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn a_file_handed_over_opens_with_its_words() {
        let editor = Editor::new();
        let file = editor
            .device
            .keep("main.rs", "text/rust", b"fn main() {}\n")
            .expect("kept");
        editor.call(
            "open_file",
            vec![Value::resource(&editor.root), Value::file(file)],
        );
        let files = editor.records(&editor.root, "files");
        assert_eq!(files.len(), 1);
        assert_eq!(editor.text(&files[0], "text"), "fn main() {}\n");
        assert_eq!(editor.text(&files[0], "language"), "Rust");
    }

    #[test]
    fn the_window_shows_the_tree_the_code_and_the_files_state() {
        let editor = Editor::new();
        let _main = editor.file("main.rs", None);
        let window = editor.window();
        let tree = window.tree();
        assert_eq!(
            of_kind(tree, "ui.TreeView").len(),
            1,
            "the project's tree stands in the sidebar"
        );
        assert!(
            !of_kind(tree, "ui.CodeEditor").is_empty(),
            "the file open is code beside it"
        );
        let status = of_kind(tree, "ui.Text")
            .into_iter()
            .map(|t| text_prop(t, "value"))
            .collect::<Vec<_>>();
        assert!(
            status.iter().any(|s| s.contains("Ln 1")),
            "the status line says where the caret is: {status:?}"
        );
    }

    #[test]
    fn a_line_asked_for_is_the_one_the_editor_shows() {
        let editor = Editor::new();
        let main = editor.file("main.rs", None);
        editor.call(
            "replace_text",
            vec![Value::resource(&main), Value::text("one\ntwo\nthree")],
        );
        editor.call(
            "reveal_line",
            vec![Value::resource(&main), Value::Int(2), Value::Int(0)],
        );
        assert_eq!(editor.number(&main, "reveal_line"), 2);
        assert_eq!(editor.number(&main, "reveal_ask"), 1);
        let window = editor.window();
        let node = of_kind(window.tree(), "ui.CodeEditor")
            .into_iter()
            .next()
            .expect("the code beside the tree");
        let reveal = match node.props.get("reveal") {
            Some(Value::Text(place)) => place.clone(),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Text(place) => place.clone(),
                other => panic!("reveal reads {other:?}"),
            },
            other => panic!("reveal reads {other:?}"),
        };
        assert_eq!(reveal, "3:1:1", "the third line, shown once");

        // Asking again at the same place is a second ask, not the one already answered.
        editor.call(
            "reveal_line",
            vec![Value::resource(&main), Value::Int(2), Value::Int(0)],
        );
        assert_eq!(editor.number(&main, "reveal_ask"), 2);
    }

    #[test]
    fn a_file_that_does_not_read_says_why() {
        let editor = Editor::new();
        let file = editor.file("handed.rs", None);
        let _ = file;
        // The engine refuses to load a path it cannot read.
        editor.call(
            "open_folder",
            vec![Value::resource(&editor.root), Value::text("/nowhere")],
        );
        assert!(
            !editor.text(&editor.root, "trouble").is_empty()
                || editor.records(&editor.root, "files").is_empty(),
            "a folder that does not read leaves the project as it was and says so"
        );
    }

    /// The text of the window's node `kind` that carries `value` as its `prop`.
    fn node_with<'a>(tree: &'a UiNode, kind: &str, prop: &str, value: &str) -> Option<&'a UiNode> {
        of_kind(tree, kind).into_iter().find(|node| {
            node.props
                .get(prop)
                .and_then(Value::as_text)
                .is_some_and(|held| held == value)
        })
    }

    /// The key a control of the window's tree declares, by the words it shows.
    fn control_key(tree: &UiNode, kind: &str, label: &str) -> Option<String> {
        node_with(tree, kind, "label", label)
            .and_then(|node| node.props.get("key"))
            .and_then(Value::as_text)
            .map(str::to_owned)
    }

    /// The view state `name` of the window's `project` view, as the window keeps it for a person.
    fn project_state(
        editor: &Editor,
        window: &tilolang_runtime::ViewInstance,
        name: &str,
    ) -> Value {
        let state = editor
            .app
            .meta()
            .views
            .iter()
            .find(|view| view.name == "project")
            .and_then(|view| view.state.iter().find(|state| state.name == name))
            .unwrap_or_else(|| panic!("the project declares `{name}`"));
        window
            .view_state(&state.id)
            .unwrap_or_else(|| panic!("the window has no `{name}`"))
    }

    /// Every command the editor's own editor answers to a key: the commands a window runs are the
    /// actions' published keys, so the palette and the window's bar answer them too; the commands
    /// that run on the open file are the keys of the controls that run them.
    #[test]
    fn every_command_of_the_editor_answers_to_a_key() {
        let editor = Editor::new();
        // Actions of the project, whose parameters the window supplies: their keys are published
        // with them.
        for (action, key) in [
            ("new_file", "Ctrl+n"),
            ("save_all", "Ctrl+Alt+s"),
            ("show_problems", "Ctrl+Shift+m"),
            ("show_terminal", "Ctrl+j"),
            ("show_search_results", "Ctrl+Shift+f"),
        ] {
            let meta = editor
                .app
                .meta()
                .action_named(action)
                .unwrap_or_else(|| panic!("no action {action}"));
            assert_eq!(
                meta.key.as_deref(),
                Some(key),
                "{action} answers to its key"
            );
        }

        let main = editor.file("main.rs", None);
        editor.call(
            "replace_text",
            vec![
                Value::resource(&main),
                Value::text("fn main() {}\nlet x = 1;\n"),
            ],
        );
        let mut window = editor.window();
        the_file_menu_answers_to_its_keys(&window);
        the_line_commands_work_on_the_caret_line(&editor, &main, &mut window);
        go_to_line_shows_the_line_the_field_holds(&editor, &main, &mut window);
        the_windows_keys_switch_the_panel_and_escape_returns(&editor, &mut window);
    }

    /// The commands on the open file are the file menu's: each answers to its key while the
    /// window has the keyboard.
    fn the_file_menu_answers_to_its_keys(window: &tilolang_runtime::ViewInstance) {
        for (label, key) in [
            ("Save", "Ctrl+S"),
            ("Save as…", "Ctrl+Shift+S"),
            ("Reload from disk", "Ctrl+Shift+R"),
            ("Format", "Ctrl+Shift+I"),
            ("Duplicate line", "Shift+Alt+Down"),
            ("Move line up", "Alt+Up"),
            ("Move line down", "Alt+Down"),
            ("Delete line", "Ctrl+Shift+K"),
            ("Toggle comment", "Ctrl+/"),
            ("Suggest", "Ctrl+Space"),
            ("Go to definition", "F12"),
            ("Open a folder…", "Ctrl+Shift+O"),
            ("Open a file…", "Ctrl+O"),
        ] {
            assert_eq!(
                control_key(window.tree(), "ui.MenuItem", label).as_deref(),
                Some(key),
                "`{label}` answers to its key: {}",
                crate::common::dump(window.tree())
            );
        }
        // Go to line puts the keyboard in the line field.
        assert_eq!(
            node_with(window.tree(), "ui.TextInput", "placeholder", "Line")
                .and_then(|node| node.props.get("key"))
                .and_then(Value::as_text),
            Some("Ctrl+G"),
            "the line field answers to Ctrl+G"
        );
    }

    /// The comment toggle and the line commands work on the line the caret is on.
    fn the_line_commands_work_on_the_caret_line(
        editor: &Editor,
        main: &ResourceRef,
        window: &mut tilolang_runtime::ViewInstance,
    ) {
        // The comment toggle the key runs comments the line the caret is on.
        let toggle = node_with(window.tree(), "ui.MenuItem", "label", "Toggle comment")
            .expect("the file menu offers the comment toggle")
            .key
            .clone();
        window.fire(&toggle, "choose").expect("the comment toggles");
        window.poll_patches().expect("the window draws the comment");
        assert_eq!(editor.text(main, "text"), "// fn main() {}\nlet x = 1;\n");

        // The line commands the keys run work on the line the caret is on.
        editor.call(
            "replace_text",
            vec![Value::resource(main), Value::text("one\ntwo\nthree\n")],
        );
        let fire = |window: &tilolang_runtime::ViewInstance, label: &str| {
            let key = node_with(window.tree(), "ui.MenuItem", "label", label)
                .unwrap_or_else(|| panic!("the file menu offers `{label}`"))
                .key
                .clone();
            window.fire(&key, "choose").expect(label);
        };
        fire(window, "Move line down");
        window.poll_patches().expect("the window draws the move");
        assert_eq!(editor.text(main, "text"), "two\none\nthree\n");
        // The caret is on the first line, so the other direction moves the second one up.
        editor.call(
            "move_line",
            vec![Value::resource(main), Value::Int(1), Value::Bool(true)],
        );
        assert_eq!(editor.text(main, "text"), "one\ntwo\nthree\n");
        fire(window, "Duplicate line");
        window.poll_patches().expect("the window draws the copy");
        assert_eq!(editor.text(main, "text"), "one\none\ntwo\nthree\n");
        fire(window, "Delete line");
        window.poll_patches().expect("the window draws the delete");
        assert_eq!(editor.text(main, "text"), "one\ntwo\nthree\n");
    }

    /// Go to line shows the line the field holds.
    fn go_to_line_shows_the_line_the_field_holds(
        editor: &Editor,
        main: &ResourceRef,
        window: &mut tilolang_runtime::ViewInstance,
    ) {
        // Go to line shows the line the field holds.
        let goto = editor
            .app
            .meta()
            .views
            .iter()
            .find(|view| view.name == "project")
            .and_then(|view| view.state.iter().find(|state| state.name == "goto"))
            .expect("the project declares `goto`")
            .id
            .clone();
        window.set_view_state(&goto, Value::text("3"));
        let go = node_with(window.tree(), "ui.Button", "label", "Go to line")
            .expect("the page offers the go-to button")
            .key
            .clone();
        window.fire(&go, "press").expect("the line is shown");
        window.poll_patches().expect("the window draws the line");
        assert_eq!(editor.number(main, "reveal_line"), 2);
    }

    /// The window's keys switch the panel under the code, and Escape in the terminal returns.
    fn the_windows_keys_switch_the_panel_and_escape_returns(
        editor: &Editor,
        window: &mut tilolang_runtime::ViewInstance,
    ) {
        // The window's keys switch the panel under the code, through the buttons its actions
        // stand on.
        for (label, panel) in [
            ("Show the terminal", "terminal"),
            ("Show the search results", "search"),
            ("Show the problems", "problems"),
        ] {
            let button = node_with(window.tree(), "ui.Button", "label", label)
                .unwrap_or_else(|| {
                    panic!(
                        "the window offers `{label}`: {}",
                        crate::common::dump(window.tree())
                    )
                })
                .key
                .clone();
            window.fire(&button, "press").expect("the panel switches");
            window.poll_patches().expect("the window draws the panel");
            match project_state(editor, window, "panel") {
                Value::Opt(Some(held)) => assert_eq!(*held, Value::text(panel)),
                Value::Text(shown) => assert_eq!(shown, panel),
                other => panic!("the panel reads {other:?}"),
            }
        }

        // Escape in the terminal's command line leaves it and returns to the code.
        let terminal = node_with(window.tree(), "ui.Button", "label", "Show the terminal")
            .expect("the window offers the terminal")
            .key
            .clone();
        window.fire(&terminal, "press").expect("the terminal shows");
        window
            .poll_patches()
            .expect("the window draws the terminal");
        let field = node_with(
            window.tree(),
            "ui.TextInput",
            "placeholder",
            "Run a command in the project",
        )
        .unwrap_or_else(|| {
            panic!(
                "the terminal has a command line: {}",
                crate::common::dump(window.tree())
            )
        })
        .key
        .clone();
        window
            .fire(&field, "cancel")
            .expect("Escape leaves the field");
        window.poll_patches().expect("the window draws the code");
        match project_state(editor, window, "panel") {
            Value::Opt(Some(held)) => assert_eq!(*held, Value::text("")),
            Value::Text(shown) => assert_eq!(shown, ""),
            other => panic!("the panel reads {other:?}"),
        }
    }

    /// The number a node's property holds, which must be a float.
    fn float_prop(node: &UiNode, name: &str) -> f64 {
        match node.props.get(name) {
            Some(Value::Float(n)) => *n,
            Some(Value::Int(n)) => f64::from(i32::try_from(*n).unwrap_or(0)),
            other => panic!("{name} reads {other:?}"),
        }
    }

    /// The editor's own text size, as the code beside the tree draws it.
    fn code_zoom(window: &tilolang_runtime::ViewInstance) -> f64 {
        let code = of_kind(window.tree(), "ui.CodeEditor")
            .into_iter()
            .next()
            .expect("the open file is drawn as code");
        float_prop(code, "zoom")
    }

    /// The code reads larger in steps the person chooses, and the size they chose is their own:
    /// kept in a record only they read, so it follows them to their other devices and changes
    /// nobody else's.
    #[test]
    fn the_editors_text_zooms_and_remembers_its_size() {
        let editor = Editor::new();
        for (action, key) in [
            ("zoom_in", "Ctrl+="),
            ("zoom_out", "Ctrl+-"),
            ("zoom_reset", "Ctrl+0"),
        ] {
            let meta = editor
                .app
                .meta()
                .action_named(action)
                .unwrap_or_else(|| panic!("no action {action}"));
            assert_eq!(
                meta.key.as_deref(),
                Some(key),
                "{action} answers to its key"
            );
        }

        let _main = editor.file("main.rs", None);
        let mut window = editor.window();
        // A person who has not chosen a size reads the code at its own size, and the header says
        // which size that is.
        assert!(
            (code_zoom(&window) - 1.0).abs() < 1e-6,
            "the code starts at its own size: {}",
            code_zoom(&window)
        );
        assert!(
            of_kind(window.tree(), "ui.MenuButton")
                .into_iter()
                .any(|node| node.props.get("label").and_then(Value::as_text) == Some("100%")),
            "the header shows the size a person chose: {}",
            crate::common::dump(window.tree())
        );
        for (label, key) in [
            ("Zoom in", "Ctrl+="),
            ("Zoom out", "Ctrl+-"),
            ("Actual size", "Ctrl+0"),
        ] {
            assert_eq!(
                control_key(window.tree(), "ui.MenuItem", label).as_deref(),
                Some(key),
                "`{label}` answers to its key"
            );
        }

        // One step larger is the next level, on the code and in the person's own setting.
        editor.call("zoom_in", vec![Value::resource(&editor.root)]);
        window
            .poll_patches()
            .expect("the window draws the larger code");
        assert!(
            (code_zoom(&window) - 1.1).abs() < 1e-6,
            "one step larger: {}",
            code_zoom(&window)
        );
        let settings = editor.records(&editor.root, "settings");
        assert_eq!(settings.len(), 1, "the person has one settings record");
        assert!(
            (editor.float(&settings[0], "editor_zoom") - 1.1).abs() < 1e-6,
            "the setting moved one step: {}",
            editor.float(&settings[0], "editor_zoom")
        );

        // The steps stop at the toolkit's ends, in both directions.
        for _ in 0..10 {
            editor.call("zoom_in", vec![Value::resource(&editor.root)]);
        }
        window
            .poll_patches()
            .expect("the window draws the largest code");
        assert!(
            (code_zoom(&window) - 3.0).abs() < 1e-6,
            "the largest size: {}",
            code_zoom(&window)
        );
        editor.call("zoom_reset", vec![Value::resource(&editor.root)]);
        window
            .poll_patches()
            .expect("the window draws its own size again");
        assert!(
            (code_zoom(&window) - 1.0).abs() < 1e-6,
            "reset is the code's own size: {}",
            code_zoom(&window)
        );
        for _ in 0..10 {
            editor.call("zoom_out", vec![Value::resource(&editor.root)]);
        }
        window
            .poll_patches()
            .expect("the window draws the smallest code");
        assert!(
            (code_zoom(&window) - 0.5).abs() < 1e-6,
            "the smallest size: {}",
            code_zoom(&window)
        );

        // The size is the person's: their settings record is the one they read, and no other
        // member is admitted to it.
        let settings = editor.records(&editor.root, "settings");
        assert_eq!(settings.len(), 1, "one person chose a size");
        assert!(
            editor.rt.readers_admit(&me(), &settings[0]),
            "the person reads their own editor size"
        );
        assert!(
            !editor.rt.readers_admit(&person("bo"), &settings[0]),
            "another member does not read this person's editor size"
        );
    }

    /// Quick open is the system's search, and it reaches this app's: what the window's search
    /// holds finds the places in the open file and searches the project's files, so the app
    /// answers the platform's search rather than keeping one of its own.
    #[test]
    fn quick_open_reaches_the_editors_search() {
        let editor = Editor::new();
        let main = editor.file("main.rs", None);
        editor.call(
            "replace_text",
            vec![
                Value::resource(&main),
                Value::text("fn other() {}\nlet held = main();\n"),
            ],
        );
        let mut window = editor.window();
        let shell = of_kind(window.tree(), "ui.NavShell")[0].key.clone();
        window
            .fire_with(&shell, "search", Some(&Value::text("main")))
            .expect("the window's search reaches the app");
        window.poll_patches().expect("the window draws the places");
        let found = editor.list(&editor.root, "found");
        assert_eq!(found.len(), 1, "the open file's place is found: {found:?}");
        assert!(
            editor
                .engine
                .asks("search")
                .iter()
                .any(|ask| ask.contains("\"needle\":\"main\"")),
            "the project is searched for the same words: {:?}",
            editor.engine.asks("search")
        );
    }
}
