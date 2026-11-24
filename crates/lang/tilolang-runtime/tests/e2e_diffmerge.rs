//! Diff & Merge (`apps/tl/diffmerge/app.tl`) end to end: two texts aligned and navigated, three
//! merged and composed as each side is taken, the merged result saved, two folders compared, and
//! the sample a person sees first. The device's work is the real engine
//! (`crates/engines/diffmerge-engine`), so the test goes through the same answers a session gives.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/diffmerge/app.tl");

    /// The device's content store, as a window lends it, kept for the test's length.
    #[derive(Clone)]
    struct Device {
        store: tilo_content::ContentStore,
        /// The window's scope: the engine's calls run inside it, as a session runs an app's.
        scope: Rc<tilo_wake::WakeScope>,
        saved: Rc<RefCell<HashMap<String, Vec<u8>>>>,
    }

    impl Device {
        fn new() -> Self {
            let store = tilo_content::ContentStore::memory_in_workspace();
            let scope = Rc::new(tilo_wake::WakeScope::new());
            tilo_resource::content::register(&scope.handle(), store.clone());
            Self {
                store,
                scope,
                saved: Rc::default(),
            }
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.store.read(hash)
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
            self.saved
                .borrow_mut()
                .insert(request.name.clone(), content.to_vec());
            Ok(())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = self.store.put(bytes).map_err(|error| error.to_string())?;
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    /// The device's engine, answered by the crate the session registers, inside the window's
    /// content store as a session answers it.
    #[derive(Clone)]
    struct Engine {
        engine: Rc<diffmerge_engine::DiffmergeEngine>,
        scope: Rc<tilo_wake::WakeScope>,
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            let _window = self.scope.enter();
            self.engine.call(op, input)
        }

        fn start(
            &self,
            _app: &str,
            op: &str,
            input: &str,
            _handlers: &tilolang_runtime::host_work::JobHandlers,
        ) -> Result<String, String> {
            let _window = self.scope.enter();
            self.engine.start(op, input)
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            let said = || -> Result<String, String> {
                let _window = self.scope.enter();
                self.engine.status(job)
            };
            Ok(said()
                .ok()
                .and_then(|said| tilolang_tilo::engine::record_of(job, &said)))
        }

        fn cancel(&self, job: &str) -> bool {
            let _window = self.scope.enter();
            self.engine.cancel(job)
        }
    }

    /// The actions that reach the device: each is granted the outside work its call makes, as a
    /// person's press of it is.
    const TOUCHING: &[&str] = &[
        "open_comparison",
        "open_files",
        "open_one",
        "chosen_left",
        "chosen_right",
        "open_merge",
        "take_ours",
        "take_theirs",
        "take_both",
        "take",
        "resolve",
        "compose_into",
        "refresh_comparison",
        "refresh_merge",
        "compare_folders",
        "chosen_folder_left",
        "chosen_folder_right",
        "hold_folders",
        "open_folder_entry",
        "save_merged",
        "save_bytes",
        "load_sample",
        "fill_sample",
        "sample_folders",
    ];

    struct Desk {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Device,
        directory: tempfile::TempDir,
    }

    impl Desk {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device::new();
            rt.set_blobs(device.clone());
            rt.set_engine(Engine {
                engine: Rc::new(diffmerge_engine::DiffmergeEngine::new()),
                scope: device.scope.clone(),
            });
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            for action in TOUCHING {
                authority.grant_outside(&app.action(action));
            }
            let root = rt.create_root("Library").unwrap();
            Self {
                rt,
                app,
                root,
                device,
                directory: tempfile::tempdir().unwrap(),
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// A call the app may refuse: the notice it leaves on `s` when it does.
        fn refused(&self, action: &str, s: &ResourceRef, args: Vec<Value>) -> String {
            self.call(action, args);
            self.text(s, "notice")
        }

        /// A file on this device, holding `text`, and the path it stands at.
        fn file(&self, name: &str, text: &str) -> String {
            let path = self.directory.path().join(name);
            std::fs::write(&path, text).unwrap();
            path.to_str().unwrap().to_owned()
        }

        /// A folder on this device, holding `files`.
        fn folder(&self, name: &str, files: &[(&str, &str)]) -> String {
            let root = self.directory.path().join(name);
            for (path, text) in files {
                let file = root.join(path);
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(file, text).unwrap();
            }
            root.to_str().unwrap().to_owned()
        }

        /// Every comparison the workspace holds, newest last.
        fn sessions(&self) -> Vec<ResourceRef> {
            match self.rt.read(&self.root, "sessions").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Ref(r) => Rc::unwrap_or_clone(r.target),
                        other => panic!("a session is a record, not {other:?}"),
                    })
                    .collect(),
                other => panic!("sessions reads {other:?}"),
            }
        }

        /// The comparison an action just made.
        fn latest(&self) -> ResourceRef {
            self.sessions().pop().expect("an action made a comparison")
        }

        /// A comparison a person started, which the texts are opened into.
        fn session(&self) -> ResourceRef {
            self.call("new_comparison", vec![Value::resource(&self.root)]);
            self.latest()
        }

        /// A comparison of two texts, opened by what a script or an agent calls.
        fn compare(&self, left: &str, right: &str) -> ResourceRef {
            let s = self.session();
            returned_ref(self.call(
                "open_comparison",
                vec![
                    Value::resource(&s),
                    Value::text("a.rs"),
                    Value::text(left),
                    Value::text("b.rs"),
                    Value::text(right),
                ],
            ))
        }

        /// A merge of three texts.
        fn merge(&self, base: &str, ours: &str, theirs: &str) -> ResourceRef {
            let s = self.session();
            returned_ref(self.call(
                "open_merge",
                vec![
                    Value::resource(&s),
                    Value::text("config.toml (base)"),
                    Value::text(base),
                    Value::text("config.toml (ours)"),
                    Value::text(ours),
                    Value::text("config.toml (theirs)"),
                    Value::text(theirs),
                ],
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
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(on) => on,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn records(&self, r: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(r, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Ref(r) => Rc::unwrap_or_clone(r.target),
                        other => panic!("{field} holds a record, not {other:?}"),
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn blocks(&self, s: &ResourceRef) -> Vec<ResourceRef> {
            self.records(s, "blocks")
        }

        fn hunks(&self, s: &ResourceRef) -> Vec<ResourceRef> {
            self.records(s, "hunks")
        }

        /// The hunk a merge's change is taken by: its place among the merge's own hunks, which
        /// counts the clean runs the app does not list.
        fn at(&self, hunk: &ResourceRef) -> Value {
            Value::Int(self.number(hunk, "index"))
        }

        fn window(&self, s: &ResourceRef) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("session_window"),
                    vec![Value::resource(s)],
                )
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }
    }

    /// The first node of `kind` in `node`'s tree.
    fn find<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
        if node.kind == kind {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, kind))
    }

    /// Every section's title in `node`'s tree.
    fn sections(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Section" {
            if let Some(Value::Opt(Some(value))) = node.props.get("title") {
                if let Value::Text(text) = &**value {
                    out.push(text.clone());
                }
            } else if let Some(Value::Text(text)) = node.props.get("title") {
                out.push(text.clone());
            }
        }
        for child in &node.children {
            sections(child, out);
        }
    }

    /// What the bar under the window says.
    fn status(window: &ViewInstance) -> String {
        match find(window.tree(), "ui.StatusLine").and_then(|bar| bar.props.get("text")) {
            Some(Value::Text(text)) => text.clone(),
            other => panic!("the status line reads {other:?}"),
        }
    }

    /// The key a control of the window declares.
    fn key_of(node: &UiNode) -> String {
        match node.props.get("key") {
            Some(Value::Text(key)) => key.clone(),
            other => panic!("{} declares no key: {other:?}", node.kind),
        }
    }

    /// The menu the window calls `label` (File, View).
    fn menu<'a>(tree: &'a UiNode, label: &str) -> &'a UiNode {
        of_kind(tree, "ui.MenuButton")
            .into_iter()
            .find(|menu| text_prop(menu, "label") == label)
            .unwrap_or_else(|| panic!("no {label} menu in the window"))
    }

    /// The item `label` of `menu`.
    fn item<'a>(menu: &'a UiNode, label: &str) -> &'a UiNode {
        of_kind(menu, "ui.MenuItem")
            .into_iter()
            .find(|item| text_prop(item, "label") == label)
            .unwrap_or_else(|| panic!("no {label} item in {}", text_prop(menu, "label")))
    }

    /// The keys a row answers to.
    fn keys_of(node: &UiNode) -> Vec<String> {
        match node.props.get("keys") {
            Some(Value::List(keys)) => keys
                .iter()
                .map(|key| match key {
                    Value::Text(key) => key.clone(),
                    other => panic!("a key is text, not {other:?}"),
                })
                .collect(),
            other => panic!("{} carries no keys: {other:?}", node.kind),
        }
    }

    tilo_test_env::scenario! {
        fn two_texts_compare_three_merge_and_both_folders_read;

        phase two_texts_align_and_count_their_changes;
        {
            let s = Desk::new();
            let d = s.compare(
                "use ui::time::Instant;\n\nstruct Limit {\n    tokens: u32,\n}\n",
                "use ui::time::Instant;\n\nstruct Limit {\n    tokens: f64,\n}\n\nfn refill() {}\n",
            );
            assert!(s.flag(&d, "compared"));
            assert_eq!(s.number(&d, "modified"), 1);
            assert_eq!(s.number(&d, "added"), 2, "the new line and the blank one");
            assert_eq!(s.number(&d, "removed"), 0);
            assert_eq!(s.text(&d, "name"), "a.rs and b.rs");
            let aligned = s.text(&d, "left_aligned").split('\n').count();
            assert_eq!(
                s.text(&d, "right_aligned").split('\n').count(),
                aligned,
                "both columns stand level"
            );
            let kinds = s.text(&d, "kinds");
            assert_eq!(kinds.len(), aligned, "one character per aligned row");
            assert!(kinds.contains('m'), "{kinds:?}");
            assert!(kinds.contains('a'), "{kinds:?}");
            let blocks = s.blocks(&d);
            assert_eq!(blocks.len(), 2, "the edited line and the added one");
            assert_eq!(s.text(&blocks[0], "kind"), "modified");
            assert_eq!(s.text(&blocks[1], "preview"), "fn refill() {}");
        }

        phase the_files_a_comparison_reads_stand_on_the_device;
        {
            let s = Desk::new();
            let left = s.file("left.rs", "one\ntwo\n");
            let right = s.file("right.rs", "one\nTWO\n");
            let d = returned_ref(s.call(
                "open_files",
                vec![
                    Value::resource(&s.root),
                    Value::text(left.as_str()),
                    Value::text(right.as_str()),
                ],
            ));
            assert_eq!(s.text(&d, "left_label"), "left.rs");
            assert_eq!(s.text(&d, "right_label"), "right.rs");
            assert_eq!(s.number(&d, "modified"), 1);
            assert_eq!(s.text(&d, "name"), "left.rs and right.rs");
            let same = returned_ref(s.call(
                "open_files",
                vec![
                    Value::resource(&s.root),
                    Value::text(left.as_str()),
                    Value::text(left.as_str()),
                ],
            ));
            assert!(s.flag(&same, "identical"));
            let missing = returned_ref(s.call(
                "open_files",
                vec![
                    Value::resource(&s.root),
                    Value::text("/no/such/file.txt"),
                    Value::text(right.as_str()),
                ],
            ));
            assert_eq!(s.text(&missing, "notice_kind"), "warning");
            assert!(!s.flag(&missing, "compared"));
        }

        phase the_window_at_a_change_and_navigating_between_them;
        {
            let s = Desk::new();
            let d = s.compare("a\nb\nc\nd\ne\n", "a\nB\nc\nD\ne\n");
            let window = s.window(&d);
            let grid = find(window.tree(), "ui.DiffGrid").expect("the grid stands in the window");
            let at = grid.props.get("at");
            assert!(
                matches!(at, Some(Value::Int(1))),
                "the window opens at the first change: {at:?}"
            );
            if let Some(Value::List(kinds)) = grid.props.get("left") {
                assert_eq!(kinds.len(), 5, "one left line per aligned row");
            }
            let said = status(&window);
            assert!(said.contains("modified"), "{said}");
            assert!(said.contains("Change 1 of"), "{said}");
        }

        phase three_texts_merge_and_a_side_is_taken;
        {
            let s = Desk::new();
            let d = s.merge("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
            assert!(s.flag(&d, "merging"));
            assert_eq!(s.number(&d, "changes"), 1);
            assert_eq!(s.number(&d, "conflicts"), 1);
            let hunks = s.hunks(&d);
            assert_eq!(hunks.len(), 1, "only the change is listed");
            assert_eq!(s.text(&hunks[0], "resolution"), "conflict");
            assert_eq!(s.text(&hunks[0], "ours"), "OURS");
            assert_eq!(s.text(&d, "merged"), "a\nOURS\nc\n", "ours until decided");
            let at = s.at(&hunks[0]);
            s.call("take_theirs", vec![Value::resource(&d), at.clone()]);
            assert_eq!(s.text(&d, "merged"), "a\nTHEIRS\nc\n");
            assert_eq!(s.number(&d, "conflicts"), 0);
            assert_eq!(s.text(&s.hunks(&d)[0], "resolution"), "theirs");
            s.call("take_both", vec![Value::resource(&d), at.clone()]);
            assert_eq!(s.text(&d, "merged"), "a\nOURS\nTHEIRS\nc\n");
            s.call(
                "resolve",
                vec![Value::resource(&d), at, Value::text("mine\nlines")],
            );
            assert_eq!(s.text(&d, "merged"), "a\nmine\nlines\nc\n");
            assert_eq!(s.text(&s.hunks(&d)[0], "resolution"), "written");
            let refused = s.refused("take_ours", &d, vec![Value::resource(&d), Value::Int(9)]);
            assert!(refused.contains("no change 10"), "{refused}");
            assert_eq!(s.text(&d, "merged"), "a\nmine\nlines\nc\n");
        }

        phase a_hand_edited_merged_result_takes_no_side;
        {
            let s = Desk::new();
            let d = s.merge("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
            let at = s.at(&s.hunks(&d)[0]);
            s.call("edit_merged", vec![Value::resource(&d), Value::text("typed by hand\n")]);
            let refused = s.refused("take_ours", &d, vec![Value::resource(&d), at]);
            assert!(
                refused.contains("edited by hand"),
                "a hand edit is never thrown away: {refused}"
            );
            assert_eq!(s.text(&d, "merged"), "typed by hand\n");
            assert_eq!(s.text(&s.hunks(&d)[0], "resolution"), "conflict");
        }

        phase the_merged_result_is_saved_where_a_person_points;
        {
            let s = Desk::new();
            let d = s.merge("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
            let path = s.directory.path().join("merged.toml");
            s.call(
                "save_merged",
                vec![
                    Value::resource(&d),
                    Value::text(path.to_str().unwrap()),
                ],
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "a\nOURS\nc\n",
                "the merged result as it stands"
            );
            assert_eq!(s.text(&d, "saved_name"), "merged.toml");
            assert_eq!(s.text(&d, "saved"), "a\nOURS\nc\n");
            let at = s.at(&s.hunks(&d)[0]);
            s.call("take_theirs", vec![Value::resource(&d), at]);
            assert_ne!(s.text(&d, "saved"), s.text(&d, "merged"), "the window knows it changed");
            let asked = match s.call("save_bytes", vec![Value::resource(&d)]) {
                InvokeOutcome::Done(Value::File(file)) => (*file).clone(),
                other => panic!("save_bytes answers a file: {other:?}"),
            };
            assert_eq!(asked.name, "config.toml");
            assert_eq!(
                s.device.store.read(&asked.hash).unwrap(),
                b"a\nTHEIRS\nc\n"
            );
        }

        phase two_folders_compare_by_what_each_holds_and_a_file_opens_on_both_sides;
        {
            let s = Desk::new();
            let left = s.folder(
                "left",
                &[("kept.rs", "same\n"), ("edited.rs", "one\n"), ("gone.rs", "gone\n")],
            );
            let right = s.folder(
                "right",
                &[("kept.rs", "same\n"), ("edited.rs", "two\n"), ("new.rs", "new\n")],
            );
            let d = s.compare("a\n", "b\n");
            s.call(
                "compare_folders",
                vec![
                    Value::resource(&d),
                    Value::text(left.as_str()),
                    Value::text(right.as_str()),
                ],
            );
            assert!(s.flag(&d, "folding"), "notice: {}", s.text(&d, "notice"));
            assert_eq!(s.text(&d, "folder_left_label"), "left");
            assert_eq!(s.text(&d, "folder_right_label"), "right");
            let entries = s.records(&d, "folder_entries");
            assert_eq!(entries.len(), 3);
            let listed: Vec<(String, String)> = entries
                .iter()
                .map(|entry| (s.text(entry, "path"), s.text(entry, "kind")))
                .collect();
            assert_eq!(
                listed,
                vec![
                    ("edited.rs".to_owned(), "modified".to_owned()),
                    ("gone.rs".to_owned(), "added".to_owned()),
                    ("new.rs".to_owned(), "removed".to_owned()),
                ]
            );
            // The changed file opens on both of its sides.
            let opened = returned_ref(s.call(
                "open_folder_entry",
                vec![
                    Value::resource(&d),
                    Value::resource(&entries[0]),
                ],
            ));
            assert_eq!(s.text(&opened, "left_label"), "edited.rs");
            assert_eq!(s.number(&opened, "modified"), 1);
            // A file only one tree holds opens against nothing.
            let one = returned_ref(s.call(
                "open_folder_entry",
                vec![
                    Value::resource(&d),
                    Value::resource(&entries[1]),
                ],
            ));
            assert_eq!(s.text(&one, "right_label"), "(no such file)");
            assert_eq!(s.number(&one, "removed"), 1);
        }

        phase the_folders_a_person_chooses_are_the_two_that_compare;
        {
            let s = Desk::new();
            let left = s.folder("pick-left", &[("a.txt", "one\n")]);
            let right = s.folder("pick-right", &[("a.txt", "two\n")]);
            let d = s.compare("x\n", "y\n");
            s.call("chosen_folder_left", vec![Value::resource(&d), Value::text(left.as_str())]);
            assert_eq!(s.text(&d, "folder_choice_left"), left);
            s.call("chosen_folder_right", vec![Value::resource(&d), Value::text(right.as_str())]);
            assert!(s.flag(&d, "folding"), "notice: {}", s.text(&d, "notice"));
            assert_eq!(s.records(&d, "folder_entries").len(), 1);
        }

        phase the_sample_opens_a_comparison_and_a_merge;
        {
            let s = Desk::new();
            let d = returned_ref(s.call("load_sample", vec![Value::resource(&s.root)]));
            s.call("refresh_comparison", vec![Value::resource(&d)]);
            let notice = s.text(&d, "notice");
            assert!(s.flag(&d, "compared"), "notice: {notice}");
            assert!(s.flag(&d, "merging"), "notice: {notice}");
            assert!(
                s.number(&d, "modified") > 0,
                "modified {} of {} / {}: notice {notice}; left {} right {}; blocks {}; kinds {:?}",
                s.number(&d, "modified"),
                s.number(&d, "added"),
                s.number(&d, "removed"),
                s.text(&d, "left_label"),
                s.text(&d, "right_label"),
                s.records(&d, "blocks").len(),
                s.text(&d, "kinds")
            );
            assert!(s.number(&d, "conflicts") > 0, "the sample has a conflict to resolve");
            assert_eq!(s.text(&d, "name"), "Merge of config.toml");
        }

        phase the_window_shows_the_sides_the_changes_and_the_merged_result;
        {
            let s = Desk::new();
            let d = returned_ref(s.call("load_sample", vec![Value::resource(&s.root)]));
            let window = s.window(&d);
            let said = status(&window);
            assert!(said.contains("changes") && said.contains("conflicts left"), "{said}");
            let mut titles = Vec::new();
            sections(window.tree(), &mut titles);
            assert!(
                titles.iter().any(|t| t.starts_with("Merged result")),
                "{titles:?}"
            );
            let card = find(window.tree(), "ui.Card");
            assert!(card.is_some(), "each change stands in a card the person decides on");
        }
    }

    tilo_test_env::scenario! {
        fn every_decision_answers_its_key_and_every_file_its_colours;

        phase the_save_action_publishes_the_windows_save_key;
        {
            let s = Desk::new();
            let save = s
                .app
                .meta()
                .action_named("save_merged")
                .expect("save_merged is published");
            assert_eq!(
                save.key.as_deref(),
                Some("Ctrl+s"),
                "the window answers the command modifier and S"
            );
        }

        phase the_merge_window_binds_open_save_as_and_taking_a_side;
        {
            let s = Desk::new();
            let d = s.merge(
                "a\nb\nc\nd\ne\n",
                "a\nOURS\nc\nOURS2\ne\n",
                "a\nTHEIRS\nc\nTHEIRS2\ne\n",
            );
            let mut window = s.window(&d);
            {
                let tree = window.tree();
                let file = menu(tree, "File");
                assert_eq!(key_of(item(file, "Compare files…")), "Ctrl+O");
                assert_eq!(key_of(item(file, "Save as…")), "Ctrl+Shift+S");
                let save = of_kind(tree, "ui.Button")
                    .into_iter()
                    .find(|button| {
                        text_prop(button, "label") == "Save…" && button.props.contains_key("key")
                    })
                    .expect("the bar saves the merged result");
                assert_eq!(key_of(save), "Ctrl+S");
                // The change the window is at answers its keys; the other changes do not, so one
                // press decides one change.
                let keyed = |tree: &UiNode, label: &str| -> String {
                    of_kind(tree, "ui.Button")
                        .into_iter()
                        .find(|button| {
                            text_prop(button, "label") == label
                                && matches!(
                                    button.props.get("key"),
                                    Some(Value::Text(key)) if !key.is_empty()
                                )
                        })
                        .map(key_of)
                        .unwrap_or_default()
                };
                let said: Vec<(String, String)> = of_kind(tree, "ui.Button")
                    .into_iter()
                    .map(|button| {
                        let key = match button.props.get("key") {
                            Some(Value::Text(key)) => key.clone(),
                            other => format!("{other:?}"),
                        };
                        (text_prop(button, "label"), key)
                    })
                    .filter(|(label, _)| label.starts_with("Take"))
                    .collect();
                assert_eq!(keyed(tree, "Take ours"), "o", "{said:?}");
                assert_eq!(keyed(tree, "Take theirs"), "t", "{said:?}");
                assert_eq!(keyed(tree, "Take both"), "b", "{said:?}");
                let ours = of_kind(tree, "ui.Button")
                    .into_iter()
                    .find(|button| {
                        text_prop(button, "label") == "Take ours"
                            && button.props.get("key") == Some(&Value::text("o"))
                    })
                    .expect("the change at answers ours");
                window
                    .fire(&ours.key, "press")
                    .expect("the key's press runs");
                assert_eq!(s.text(&s.hunks(&d)[0], "resolution"), "ours");
                assert_eq!(s.text(&s.hunks(&d)[1], "resolution"), "conflict");
            }
            // The person moves to the next change with the key the View menu names; its keys are
            // the ones that answer now.
            let next = {
                let tree = window.tree();
                let view = menu(tree, "View");
                let next = item(view, "Next change");
                assert_eq!(key_of(next), "n");
                next.key.clone()
            };
            window.fire(&next, "choose").expect("the next change is chosen");
            window.poll_patches().unwrap();
            assert_eq!(of_kind(window.tree(), "ui.Card").len(), 2);
            let theirs = of_kind(window.tree(), "ui.Button")
                .into_iter()
                .find(|button| {
                    text_prop(button, "label") == "Take theirs"
                        && button.props.get("key") == Some(&Value::text("t"))
                })
                .expect("the second change answers theirs");
            window
                .fire(&theirs.key, "press")
                .expect("the key's press runs");
            assert_eq!(s.text(&s.hunks(&d)[1], "resolution"), "theirs");
            assert_eq!(s.text(&s.hunks(&d)[0], "resolution"), "ours");
        }

        phase the_comparison_window_binds_its_modes_and_navigation;
        {
            let s = Desk::new();
            let d = s.compare("a\nb\nc\n", "a\nB\nC\n");
            let window = s.window(&d);
            let tree = window.tree();
            let view = menu(tree, "View");
            assert_eq!(key_of(item(view, "Diff")), "1");
            assert_eq!(key_of(item(view, "Merge")), "2");
            assert_eq!(key_of(item(view, "Folders")), "f");
            assert_eq!(key_of(item(view, "Previous change")), "p");
            assert_eq!(key_of(item(view, "Next change")), "n");
            let buttons = of_kind(tree, "ui.Button");
            assert!(
                buttons
                    .iter()
                    .any(|button| button.props.get("key") == Some(&Value::text("F7"))),
                "F7 steps back"
            );
            assert!(
                buttons
                    .iter()
                    .any(|button| button.props.get("key") == Some(&Value::text("F8"))),
                "F8 steps on"
            );
        }

        phase a_folder_rows_enter_opens_its_file;
        {
            let s = Desk::new();
            let left = s.folder("keys-left", &[("a.txt", "one\n")]);
            let right = s.folder("keys-right", &[("a.txt", "two\n")]);
            let d = s.compare("x\n", "y\n");
            s.call(
                "compare_folders",
                vec![
                    Value::resource(&d),
                    Value::text(left.as_str()),
                    Value::text(right.as_str()),
                ],
            );
            let window = s.window(&d);
            let rows = of_kind(window.tree(), "ui.ListItem");
            assert!(!rows.is_empty(), "the differing files stand as rows");
            for row in rows {
                assert_eq!(keys_of(row), ["Enter"]);
            }
        }

        phase a_file_is_coloured_as_the_language_its_name_names;
        {
            let s = Desk::new();
            let cases = [
                ("holiday.py", "python"),
                ("app.go", "go"),
                ("notes.md", "markdown"),
                ("config.yaml", "yaml"),
                ("Dockerfile", "dockerfile"),
                ("main.rs", "rust"),
                ("index.html", "html"),
                ("page.css", "css"),
                ("data.json", "json"),
                ("query.sql", "sql"),
                ("paper.typ", "typst"),
                ("script.sh", "shell"),
                ("settings.toml", "toml"),
                ("main.c", "c"),
                ("engine.cpp", "cpp"),
                ("counter.v", "verilog"),
                ("top.sv", "system_verilog"),
                ("app.tl", "tilolang"),
                (".gitignore", "git"),
                ("Main.java", "plain_text"),
            ];
            for (name, language) in cases {
                let session = s.session();
                let d = returned_ref(s.call(
                    "open_comparison",
                    vec![
                        Value::resource(&session),
                        Value::text(name),
                        Value::text("one\n"),
                        Value::text(name),
                        Value::text("two\n"),
                    ],
                ));
                let window = s.window(&d);
                let grid = find(window.tree(), "ui.DiffGrid").expect("the comparison grid stands");
                assert_eq!(text_prop(grid, "language"), language, "{name}");
            }
        }

        phase a_merge_colours_every_side_as_the_file_it_is;
        {
            let s = Desk::new();
            for (name, language) in [
                ("holiday.py", "python"),
                ("app.go", "go"),
                ("Dockerfile", "dockerfile"),
            ] {
                let session = s.session();
                let m = returned_ref(s.call(
                    "open_merge",
                    vec![
                        Value::resource(&session),
                        Value::text(format!("{name} (base)")),
                        Value::text("one\n"),
                        Value::text(format!("{name} (ours)")),
                        Value::text("two\n"),
                        Value::text(format!("{name} (theirs)")),
                        Value::text("three\n"),
                    ],
                ));
                let window = s.window(&m);
                let editors = of_kind(window.tree(), "ui.CodeEditor");
                assert!(!editors.is_empty(), "{name} shows its sides");
                for editor in editors {
                    assert_eq!(text_prop(editor, "language"), language, "{name}");
                }
            }
        }
    }
}
