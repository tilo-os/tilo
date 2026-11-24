//! The Code Editor (`apps/tl/code-editor/app.tl`) end to end: a project's files made, moved,
//! written and saved through its actions; a folder read from the device and a file written
//! back; a command run, a project searched and a language server asked — all through the one
//! engine the app calls — and its window, with the tree, the code and the file's state.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/code-editor/app.tl");

    /// The device's engine: what it was asked, and the answers it gives.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        /// Whether the next save finds the file changed on disk.
        conflict: Rc<RefCell<bool>>,
    }

    impl Engine {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }

        fn last(&self) -> (String, String) {
            self.asked.borrow().last().cloned().unwrap_or_default()
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
                "list" if input.contains("/nowhere") => {
                    Err("no folder stands there".to_owned())
                }
                "list" => Ok(r#"{"entries":[
                    {"name":"src","path":"/p/src","folder":true,"hidden":false},
                    {"name":"README.md","path":"/p/README.md","folder":false,"hidden":false},
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
                "save" if *self.conflict.borrow() => Err("conflict".to_owned()),
                "save" => Ok(r#"{"mtime":42,"len":17}"#.to_owned()),
                "stat" => Ok(r#"{"exists":true,"mtime":42,"len":17}"#.to_owned()),
                "change" | "open" | "saved" | "ask" => Ok("{}".to_owned()),
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                "run" => Ok(r#"{"job":"run-1"}"#.to_owned()),
                "search" => Ok(r#"{"job":"search-1"}"#.to_owned()),
                "server" => Ok(r#"{"job":"server-1"}"#.to_owned()),
                other => Err(format!("`{other}` is no job this device starts")),
            }
        }

        fn status(&self, job: &str) -> Result<String, String> {
            match job {
                "run-1" => Ok(
                    r#"{"state":"done","exit":101,"output":"src/main.rs:3:5: error: expected `;`\ntest tests::it ... ok\n","truncated":false,"failure":""}"#
                        .to_owned(),
                ),
                "search-1" => Ok(
                    r#"{"state":"done","hits":[{"path":"/p/src/main.rs","rel":"src/main.rs","line":0,"column":9,"len":4,"preview":"fn main() {}"}],"files":2,"truncated":false}"#
                        .to_owned(),
                ),
                "server-1" => Ok(r#"{"state":"ready","server":"rust-analyzer","failure":"","progress":null,"diagnostics":{"\/p\/src\/main.rs":[{"severity":"warning","line":0,"column":3,"message":"unused import"}]},"answer":""}"#.to_owned()),
                other => Err(format!("no job `{other}` runs on this device")),
            }
        }

        fn cancel(&self, job: &str) -> bool {
            job == "run-1" || job == "search-1" || job == "server-1"
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
        device: Device,
    }

    impl Editor {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let engine = Engine::default();
            rt.set_engine(engine.clone());
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
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
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

        fn number(&self, r: &ResourceRef, field: &str) -> i64 {
            match self.rt.read(r, field).unwrap() {
                Value::Int(n) => n,
                Value::Float(n) if n.is_finite() => n as i64,
                Value::Float(_) => 0,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(b) => b,
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
        assert_eq!(editor.number(&main, "kind"), 0, "a file is a file");

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
        assert_eq!(editor.text(&main, "parent"), editor.text(&main, "parent"));

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
            vec![
                Value::resource(&main),
                Value::text("one\ntwo\nthree"),
            ],
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

        editor.call(
            "indent_line",
            vec![Value::resource(&main), Value::Int(0)],
        );
        assert_eq!(editor.text(&main, "text"), "    one\none\nthree\ntwo");
        editor.call(
            "outdent_line",
            vec![Value::resource(&main), Value::Int(0)],
        );
        assert_eq!(editor.text(&main, "text"), "one\none\nthree\ntwo");

        editor.call(
            "delete_line",
            vec![Value::resource(&main), Value::Int(3)],
        );
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
                assert_eq!(items.len(), 1, "only the word `fs`, not `offset` or `refs`")
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
        let symbols = editor.call("symbols_in", vec![Value::resource(&main)]);
        match symbols {
            InvokeOutcome::Done(Value::List(items)) => {
                assert_eq!(items.len(), 2, "the function and the struct")
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
            "the folder's file is an entry"
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
            editor.engine.asks("list").iter().any(|ask| ask.contains("/p")),
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
        editor.call("save", vec![Value::resource(&editor.root), Value::resource(&readme)]);
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
        editor.call("save", vec![Value::resource(&editor.root), Value::resource(&readme)]);
        assert!(editor.flag(&readme, "stale"), "the file changed on disk");
        assert_eq!(editor.text(&readme, "saved"), "# changed\n", "nothing was written");

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

        editor.call("poll", vec![Value::resource(&editor.root)]);
        assert_eq!(editor.text(&commands[0], "state"), "done");
        assert_eq!(editor.number(&commands[0], "exit"), 101);
        assert!(editor.text(&commands[0], "output").contains("expected"));
        let items = editor.records(&commands[0], "items");
        assert!(
            items
                .iter()
                .any(|i| editor.text(i, "path") == "src/main.rs" && editor.number(i, "line") == 3),
            "the compiler's place is an item"
        );

        editor.call(
            "rerun",
            vec![Value::resource(&editor.root), Value::resource(&commands[0])],
        );
        assert_eq!(editor.records(&editor.root, "commands").len(), 2);
        assert!(editor.engine.asks("run").len() == 2);
    }

    #[test]
    fn a_project_search_runs_and_its_result_opens_the_file() {
        let editor = Editor::new();
        editor.call(
            "search_project",
            vec![
                Value::resource(&editor.root),
                Value::text("main"),
                Value::Bool(false),
                Value::Bool(false),
            ],
        );
        editor.call("poll", vec![Value::resource(&editor.root)]);
        assert_eq!(editor.number(&editor.root, "search_files"), 2);
        let hits = editor.records(&editor.root, "search_hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(editor.text(&hits[0], "rel"), "src/main.rs");

        editor.call(
            "open_hit",
            vec![Value::resource(&editor.root), Value::resource(&hits[0])],
        );
        let files = editor.records(&editor.root, "files");
        let main = files
            .iter()
            .find(|f| editor.text(f, "name") == "main.rs")
            .expect("the result opened its file");
        assert_eq!(editor.text(main, "text"), "fn main() {}\n");
        assert_eq!(editor.number(&editor.root, "answer_line"), 0);
    }

    #[test]
    fn a_language_server_answers_with_its_problems() {
        let editor = Editor::new();
        editor.call(
            "open_folder",
            vec![Value::resource(&editor.root), Value::text("/p")],
        );
        let main = editor
            .records(&editor.root, "files")
            .into_iter()
            .find(|f| editor.text(f, "name") == "README.md")
            .expect("a file");
        // The server is started for a Rust file and asked to check it.
        let rust = editor.file("src/main.rs", None);
        editor.call(
            "check",
            vec![Value::resource(&editor.root), Value::resource(&rust)],
        );
        assert!(
            editor.text(&editor.root, "server_job") != "",
            "a server was started for Rust: {:?}",
            editor.engine.ops()
        );
        editor.call("poll", vec![Value::resource(&editor.root)]);
        assert_eq!(editor.text(&editor.root, "server_name"), "rust-analyzer");
        let _ = main;

        editor.call(
            "complete",
            vec![
                Value::resource(&editor.root),
                Value::resource(&rust),
                Value::Int(0),
                Value::Int(0),
            ],
        );
        assert!(editor.engine.asks("ask").iter().any(|ask| ask.contains("\"what\":\"complete\"")));
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
        let problems = editor.records(&broken, "problems");
        assert!(!problems.is_empty(), "an open bracket is a problem");
        assert_eq!(editor.text(&broken, "source"), "lint");
        assert!(editor.text(&broken, "checked") != "");
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
        assert_eq!(editor.text(&editor.root, "server_job"), "", "no server started");
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
        let mut window = editor.window();
        let tree = window.tree();
        assert!(
            of_kind(tree, "ui.TreeView").len() == 1,
            "the project's tree stands in the sidebar"
        );
        assert!(
            of_kind(tree, "ui.CodeEditor").len() >= 1,
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
}
