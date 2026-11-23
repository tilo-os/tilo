//! The notes app (`apps/tl/notes/app.tl`) end to end: a notebook whose notes are a tree.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use bounded_zip::{ZipLimits, write_zip};
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::rich::RichValue;
    use tilolang_runtime::{
        AmStore, EditOutcome, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
        ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/notes/app.tl");
    const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");

    /// The device the notebook's files are read from: its content store, of what a test kept
    /// there. A file's hash is its bytes, as a content store addresses them: the same bytes are
    /// one file, whatever name they came in as.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl Device {
        /// `bytes` kept as the file `name`: what a person choosing it would answer.
        fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
            let hash = blake3::hash(bytes).to_hex().to_string();
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            )
        }

        /// The pages `entries` as one archive, kept under `name`.
        fn archive(&self, name: &str, entries: &[(&str, &[u8])]) -> FileValue {
            let entries: Vec<(String, Vec<u8>)> = entries
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
                .collect();
            let bytes = write_zip(&entries, &ZipLimits::default()).expect("the archive writes");
            self.keeping(name, "application/zip", &bytes)
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Option<FileValue>, String> {
            Ok(None)
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            Ok(self.keeping(name, media, bytes))
        }
    }

    /// A PNG of one colour: what a page that shows a picture keeps beside it.
    fn png() -> Vec<u8> {
        // A 2x2 PNG, written out here so the test needs no image library.
        const PIXELS: &[u8] = &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x72, 0xb6, 0x0d, 0x24, 0x00, 0x00, 0x00, 0x16, b'I', b'D', b'A', b'T', 0x78,
            0x9c, 0x63, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00, 0x08, 0x00,
            0x01, 0x2c, 0x0a, 0x0b, 0x4e, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae,
            0x42, 0x60, 0x82,
        ];
        PIXELS.to_vec()
    }

    struct Notes {
        rt: Runtime,
        app: App,
        nb: ResourceRef,
        /// The device whose files the notebook reads, as a person's own store holds them.
        device: Device,
        /// What the workspace's clock says: when a note is made, and when it is written since.
        clock: Rc<RefCell<String>>,
    }

    impl Notes {
        fn new() -> Self {
            Self::at("2026-06-30T08:00:00Z")
        }

        /// A notebook whose clock says `now`.
        fn at(now: &str) -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let device = Device::default();
            rt.set_blobs(device.clone());
            let clock = Rc::new(RefCell::new(now.to_owned()));
            let reading = Rc::clone(&clock);
            rt.set_clock(move || reading.borrow().clone());
            let discussion = App::compile(DISCUSSION, &[]);
            discussion.load(&rt);
            let app = App::compile(SOURCE, &[discussion.meta().clone()]);
            app.load(&rt);
            // The notebook is the app's root, as the window a person opens on it holds it: an
            // action that names `root Notebook` and one given the notebook act on one resource.
            let nb = rt.create_root("Notebook").unwrap();
            Self {
                rt,
                app,
                nb,
                device,
                clock,
            }
        }

        /// A page of a service imported as the file `name`, saying who wrote it and where it is
        /// read: the note it made, or the one it made already.
        fn import_page(
            &self,
            name: &str,
            text: &str,
            by: &str,
            service: &str,
            page: &str,
        ) -> ResourceRef {
            let file = Value::file(self.device.keeping(name, "text/markdown", text.as_bytes()));
            returned_ref(self.call(
                "import_page",
                vec![
                    Value::resource(&self.nb),
                    file,
                    Value::text(by),
                    Value::text(service),
                    Value::text(page),
                    Value::text("2025-08-01T10:00:00Z"),
                    Value::text("2025-09-01T10:00:00Z"),
                ],
            ))
        }

        /// The Markdown file `name` imported by the person's own File menu: no service's words.
        fn import_file(&self, name: &str, text: &str) -> ResourceRef {
            let file = Value::file(self.device.keeping(name, "text/markdown", text.as_bytes()));
            returned_ref(self.call("import_markdown", vec![Value::resource(&self.nb), file]))
        }

        /// What `field` of `note` says, as text.
        fn said_of(&self, note: &ResourceRef, field: &str) -> String {
            match self.rt.read(note, field).unwrap() {
                Value::Text(said) => said,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// The clock reads `now` from here on: a change made now is later than those before it.
        fn set_clock(&self, now: &str) {
            now.clone_into(&mut self.clock.borrow_mut());
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn new_note(&self) -> ResourceRef {
            returned_ref(self.call("new_note", vec![Value::resource(&self.nb)]))
        }

        fn child_of(&self, parent: &ResourceRef) -> ResourceRef {
            returned_ref(self.call("new_child", vec![Value::resource(parent)]))
        }

        fn titled(&self, title: &str) -> ResourceRef {
            let note = self.new_note();
            self.call("retitle", vec![Value::resource(&note), Value::text(title)]);
            note
        }

        /// The notebook's window, its rows built as a host shows them: the tree holds the rows
        /// the window asked for, and none other.
        fn shown(&self) -> ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("notebook"),
                    vec![Value::resource(&self.nb)],
                )
                .unwrap()
        }

        /// The notebook's window, every row of its tree drawn.
        fn window(&self) -> ViewInstance {
            let mut view = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("notebook"),
                    vec![Value::resource(&self.nb)],
                )
                .unwrap();
            // These checks read the whole tree, as a host that draws every row does.
            view.set_lazy_rows(false).unwrap();
            view
        }

        fn parent_of(&self, note: &ResourceRef) -> Option<ResourceRef> {
            match self.rt.read(note, "parent").unwrap() {
                Value::Opt(Some(held)) => match *held {
                    Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                    other => panic!("a parent is a note, not {other:?}"),
                },
                Value::Opt(None) => None,
                other => panic!("parent reads {other:?}"),
            }
        }

        fn title_of(&self, note: &ResourceRef) -> String {
            match self.rt.read(note, "title").unwrap() {
                Value::Text(title) => title,
                other => panic!("title reads {other:?}"),
            }
        }

        /// Import the archive `file` as notes, as the notebook's File menu does.
        fn import(&self, file: &FileValue) {
            self.call(
                "import_archive",
                vec![Value::resource(&self.nb), Value::file(file.clone())],
            );
        }

        /// Every note the notebook holds, by the tree the window shows them in.
        fn notes(&self) -> Vec<ResourceRef> {
            of_kind(the_tree(&self.window()), "ui.TreeViewNode")
                .into_iter()
                .filter_map(|node| match node.props.get("item") {
                    Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .collect()
        }

        /// The titles of every note, in the order the tree shows them.
        fn titles(&self) -> Vec<String> {
            self.notes()
                .iter()
                .map(|note| self.title_of(note))
                .collect()
        }

        fn note_titled(&self, title: &str) -> ResourceRef {
            self.notes()
                .into_iter()
                .find(|note| self.title_of(note) == title)
                .unwrap_or_else(|| panic!("no note titled {title}"))
        }

        fn body_of(&self, note: &ResourceRef) -> RichValue {
            match self.rt.read(note, "body").unwrap() {
                Value::Rich(body) => body,
                other => panic!("body reads {other:?}"),
            }
        }

        /// The pictures the note keeps, each by the address of its bytes and the name it came
        /// in as: a map reads as its entries, each a pair.
        fn images_of(&self, note: &ResourceRef) -> Vec<(Value, Value)> {
            match self.rt.read(note, "images").unwrap() {
                Value::List(entries) => entries
                    .into_iter()
                    .filter_map(|entry| match entry {
                        Value::Tuple(fields) if fields.len() == 2 => {
                            let mut fields = fields.into_iter();
                            Some((fields.next()?, fields.next()?))
                        }
                        _ => None,
                    })
                    .collect(),
                other => panic!("images reads {other:?}"),
            }
        }

        /// The notes the body of `note` refers to, by the ids of the notes they name.
        fn linked_ids(&self, note: &ResourceRef) -> Vec<String> {
            self.body_of(note)
                .references()
                .iter()
                .filter(|reference| reference.as_resource().is_some())
                .filter_map(tilolang_runtime::rich::resource_id)
                .map(str::to_owned)
                .collect()
        }
    }

    /// The tree the window's sidebar is: the shelf of pinned notes, when it stands, is the
    /// first of the sidebar's trees and the notes are the last.
    fn the_tree(view: &ViewInstance) -> &UiNode {
        of_kind(view.tree(), "ui.TreeView")
            .last()
            .copied()
            .unwrap_or_else(|| panic!("no tree in\n{}", dump(view.tree())))
    }

    /// What the part `name` of the window's shell holds: a pane of it.
    fn part<'a>(shell: &'a UiNode, name: &str) -> &'a UiNode {
        shell
            .children
            .iter()
            .find(|child| {
                child.kind == "part" && child.props.get("name") == Some(&Value::text(name))
            })
            .unwrap_or_else(|| panic!("no {name} in\n{}", dump(shell)))
    }

    /// The notes the window's tree draws, in its order.
    fn labels(view: &ViewInstance) -> Vec<String> {
        of_kind(the_tree(view), "ui.TreeViewNode")
            .iter()
            .map(|node| text_prop(node, "label"))
            .collect()
    }

    /// The tree node of the note with this title.
    fn node_titled<'a>(view: &'a ViewInstance, title: &str) -> &'a UiNode {
        of_kind(the_tree(view), "ui.TreeViewNode")
            .into_iter()
            .find(|node| text_prop(node, "label") == title)
            .unwrap_or_else(|| panic!("no node {title} in\n{}", dump(view.tree())))
    }

    /// Whether the tree node's flag `name` is set: a node says it as a boolean or as one.
    fn flag(node: &UiNode, name: &str) -> bool {
        match node.props.get(name) {
            Some(Value::Bool(flag)) => *flag,
            Some(Value::Int(number)) => *number != 0,
            _ => false,
        }
    }

    /// The parent a tree node names, as the tree indents it.
    fn parent_of_node(node: &UiNode) -> Option<ResourceRef> {
        match node.props.get("parent") {
            Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                other => panic!("a node's parent is a note, not {other:?}"),
            },
            _ => None,
        }
    }

    /// The note a tree node names as the one just before it under the same parent: the place
    /// its actions get and the row `Tab` indents it under.
    fn above_of_node(node: &UiNode) -> Option<ResourceRef> {
        match node.props.get("above") {
            Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                other => panic!("a node's row above is a note, not {other:?}"),
            },
            _ => None,
        }
    }

    /// Tell the tree of a move: `event` is how the note `moved` lands — `drop` just before the
    /// row `at`, `into` at the end of its own, `after` past it and all it holds — and `at` is
    /// none where it lands at the end of the top level.
    fn move_on(view: &ViewInstance, event: &str, moved: &ResourceRef, at: Option<&ResourceRef>) {
        let key = the_tree(view).key.clone();
        // The row it lands at is the place the move's parameter holds, an option: some row, or
        // none at the end of the top level.
        let at = at.map_or_else(Value::none, |at| Value::some(Value::resource(at)));
        view.fire_with(
            &key,
            event,
            Some(&Value::List(vec![Value::resource(moved), at])),
        )
        .unwrap();
    }

    #[test]
    fn a_new_note_is_untitled() {
        let n = Notes::new();
        let note = n.new_note();
        assert_eq!(
            n.rt.read(&note, "title").unwrap(),
            Value::text(""),
            "its title starts empty: what is typed first names it"
        );
        assert_eq!(n.rt.read(&note, "pinned").unwrap(), Value::Bool(false));
        assert_eq!(n.parent_of(&note), None, "it stands at the top of the tree");
        assert_eq!(
            n.rt.title(&note).unwrap(),
            "Untitled",
            "a note with no title is called what its type says"
        );
        assert_eq!(
            n.rt.title(&n.nb).unwrap(),
            "Notes",
            "the notebook is called by the app's title"
        );
    }

    #[test]
    fn pin_tag_and_delete() {
        let n = Notes::new();
        let first = n.new_note();
        let second = n.new_note();
        n.call("pin", vec![Value::resource(&first)]);
        assert_eq!(n.rt.read(&first, "pinned").unwrap(), Value::Bool(true));
        n.call("pin", vec![Value::resource(&first)]);
        assert_eq!(n.rt.read(&first, "pinned").unwrap(), Value::Bool(false));

        n.call("tag", vec![Value::resource(&first), Value::text("work")]);
        n.call("tag", vec![Value::resource(&first), Value::text("idea")]);
        n.call("tag", vec![Value::resource(&first), Value::text("work")]);
        let tags = n.rt.read(&first, "tags").unwrap();
        assert_eq!(
            tags,
            Value::List(vec![Value::text("idea"), Value::text("work")]),
            "a set holds each tag once"
        );

        n.call(
            "delete",
            vec![Value::resource(&n.nb), Value::resource(&second)],
        );
        n.call(
            "delete",
            vec![Value::resource(&n.nb), Value::resource(&first)],
        );
        assert_eq!(
            n.rt.project(&n.rt.read(&n.nb, "notes").unwrap())["count"]
                .as_i64()
                .unwrap(),
            0
        );
    }

    /// The window is two panes: the notes as a tree in the sidebar, and the note chosen beside
    /// it — no list of them between. A window wide enough docks the tree beside the page it
    /// opens; one too narrow for both makes the tree its first screen and pushes the page in
    /// front of it, with the way back. The bar carries the "+" that starts a note, once.
    #[test]
    fn the_notebook_is_the_tree_beside_the_note_it_opens() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        n.call("pin", vec![Value::resource(&plan)]);
        let mut view = n.window();
        let shell = of_kind(view.tree(), "ui.NavShell")[0];
        assert_eq!(
            text_prop(shell, "content_kind"),
            "reading",
            "the window is a reading column: wide, the tree docks beside the page, and narrow, \
             the tree is its first screen: {}",
            dump(view.tree())
        );
        assert_eq!(text_prop(shell, "sidebar_title"), "Notes");
        // The notes stand in the sidebar and the note the window shows in the content beside
        // them: the two panes a wide window docks side by side.
        let sidebar = part(shell, "sidebar");
        let content = part(shell, "content");
        assert_eq!(
            labels(&view),
            ["Q3 plan"],
            "the tree holds the notes: {}",
            dump(view.tree())
        );
        assert!(
            of_kind(sidebar, "ui.TreeView")
                .iter()
                .any(|tree| tree.key == the_tree(&view).key),
            "the tree is the sidebar's: {}",
            dump(view.tree())
        );
        assert_eq!(
            of_kind(content, "ui.BlockEditor").len(),
            1,
            "the open note's body stands in the content, beside the tree: {}",
            dump(view.tree())
        );
        assert!(
            of_kind(sidebar, "ui.BlockEditor").is_empty(),
            "the page stands beside the tree, not in it: {}",
            dump(view.tree())
        );
        // The window's "+": the primary action that makes a note, in the window's bar, where a
        // narrow window's page keeps it too rather than in the sidebar's header.
        let head = of_kind(part(shell, "actions"), "ui.Button")
            .into_iter()
            .find(|button| text_prop(button, "label") == "New note")
            .unwrap_or_else(|| panic!("no New note button in\n{}", dump(view.tree())));
        assert!(
            of_kind(sidebar, "ui.Button")
                .iter()
                .all(|button| button.key != head.key),
            "the bar offers it once, not again over the tree: {}",
            dump(view.tree())
        );
        assert_eq!(
            text_prop(of_kind(content, "ui.TextInput")[0], "placeholder"),
            "Untitled",
            "the page edits the open note's title"
        );

        // Choosing a row opens that note in the content column.
        let home = n.titled("Home");
        view.poll_patches().unwrap();
        assert_eq!(labels(&view), ["Q3 plan", "Home"]);
        let key = the_tree(&view).key.clone();
        view.fire_with(&key, "select", Some(&Value::resource(&home)))
            .unwrap();
        view.poll_patches().unwrap();

        let title = of_kind(view.tree(), "ui.TextInput")[0].key.clone();
        assert_eq!(
            view.edit(&title, &Value::text("Flat")).unwrap(),
            EditOutcome::Applied
        );
        assert_eq!(
            n.title_of(&home),
            "Flat",
            "the page edits the note the person chose"
        );
        assert_eq!(n.title_of(&plan), "Q3 plan");
    }

    /// The tree is a list of rows built as the window shows them: opening a notebook draws no
    /// row of it, a row is read from the view when the window asks for it — of a thousand notes
    /// as of three — and what it reads says where the note stands and whether it holds any.
    #[test]
    fn the_tree_builds_the_rows_the_window_shows() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let meetings = n.child_of(&plan);
        n.call(
            "retitle",
            vec![Value::resource(&meetings), Value::text("Meetings")],
        );
        n.titled("Notes");
        let mut view = n.shown();
        let tree = the_tree(&view);
        let key = tree.key.clone();
        assert_eq!(view.list_len(&key), Some(3), "the tree holds three notes");
        assert!(
            of_kind(view.tree(), "ui.TreeViewNode").is_empty(),
            "no row is built before the window shows it: {}",
            dump(view.tree())
        );

        // The row the window shows first: the note it reads, how deep it stands, and that it
        // holds one of its own.
        let row = view.row(&key, 0).unwrap().expect("shown").clone();
        let node = of_kind(&row, "ui.TreeViewNode")[0];
        assert_eq!(text_prop(node, "label"), "Q3 plan");
        assert_eq!(
            node.props.get("depth"),
            Some(&Value::Int(0)),
            "the top row stands at the top"
        );
        assert!(flag(node, "branch"), "the note holds one of its own");

        // The second row stands under it, one deep.
        let under = view.row(&key, 1).unwrap().expect("shown").clone();
        let child = of_kind(&under, "ui.TreeViewNode")[0];
        assert_eq!(text_prop(child, "label"), "Meetings");
        assert_eq!(
            child.props.get("depth"),
            Some(&Value::Int(1)),
            "the note it holds stands one deep"
        );
        assert!(
            !flag(child, "branch"),
            "a note with none of its own holds none"
        );
        assert_eq!(parent_of_node(child), Some(plan));
    }

    /// The row over a note is the one beside it under the same parent, whatever the window shows
    /// between them: a note following an expanded branch names that branch, never none — the
    /// place its actions get and the row `Tab` indents it under.
    #[test]
    fn the_row_over_a_note_is_the_one_beside_it() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let first = n.child_of(&plan);
        let second = n.child_of(&plan);
        let after = n.titled("Groceries");
        let view = n.window();
        assert_eq!(
            above_of_node(node_titled(&view, "Groceries")),
            Some(plan.clone()),
            "the row over the note after the branch is the branch itself, whose own stand \
             between them"
        );
        let child = |id: &ResourceRef, title: &str| {
            n.call("retitle", vec![Value::resource(id), Value::text(title)]);
            id.clone()
        };
        child(&first, "Agenda");
        child(&second, "Minutes");
        let mut view = n.window();
        assert_eq!(
            above_of_node(node_titled(&view, "Minutes")),
            Some(first),
            "under the branch, the row over one note is the one before it"
        );
        assert_eq!(
            above_of_node(node_titled(&view, "Agenda")),
            None,
            "the first of them has no row of its own over it"
        );
        assert_eq!(
            above_of_node(node_titled(&view, "Q3 plan")),
            None,
            "and the first row at the top names none"
        );

        // What the tree reports of `Tab` on the note after the branch: the note lands inside
        // the branch, and the store holds it there.
        view.poll_patches().unwrap();
        move_on(&view, "into", &after, Some(&plan));
        assert_eq!(
            n.parent_of(&after),
            Some(plan),
            "Tab under the branch makes the note one of it"
        );
    }

    /// Dropping a note on a row makes it a note of that one: the tree indents it under the row
    /// it landed on, and the store holds it there.
    #[test]
    fn a_note_dropped_on_a_row_stands_inside_it() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let meetings = n.titled("Meetings");
        let mut view = n.window();
        move_on(&view, "into", &plan, Some(&meetings));
        assert_eq!(
            n.parent_of(&plan),
            Some(meetings.clone()),
            "a note dropped inside another is its own"
        );
        view.poll_patches().unwrap();
        assert_eq!(
            parent_of_node(node_titled(&view, "Q3 plan")),
            Some(meetings.clone()),
            "and the tree draws it under that row"
        );

        // Dropped inside a row, the note stands at the end of that row's own, the notes under it
        // with it: a tree keeps a child just after the note it stands in.
        let notes = n.titled("Notes");
        view.poll_patches().unwrap();
        assert_eq!(
            labels(&view),
            ["Meetings", "Q3 plan", "Notes"],
            "the tree keeps the order the store does"
        );

        // Dropped before a row, it takes that row's place among its own, with the notes under
        // it: the branch moves whole.
        move_on(&view, "drop", &notes, Some(&meetings));
        assert_eq!(n.parent_of(&notes), None);
        view.poll_patches().unwrap();
        assert_eq!(
            labels(&view),
            ["Notes", "Meetings", "Q3 plan"],
            "the tree keeps the order the store does"
        );

        // Dropped on a row that stands under another, it is a note of that row itself, never of
        // the row over it.
        move_on(&view, "into", &notes, Some(&plan));
        assert_eq!(
            n.parent_of(&notes),
            Some(plan),
            "a note dropped on a row of a branch is that row's own"
        );
    }

    /// The tree verbs a person reaches from a row's menu and an agent invokes: a note goes under
    /// the one over it, out beside the one it stands in, and over or under the one beside it, and
    /// the notes under it go with it.
    #[test]
    fn the_tree_verbs_move_a_note_and_the_notes_under_it() {
        let n = Notes::new();
        let top = n.titled("A");
        let b = n.titled("B");
        let c = n.titled("C");
        assert_eq!(n.parent_of(&top), None, "the first note stands at the top");
        let under_b = n.child_of(&b);
        n.call(
            "retitle",
            vec![Value::resource(&under_b), Value::text("B1")],
        );
        let window = |n: &Notes| labels(&n.window());

        // Indent: C goes under B, at the end of the notes B holds.
        n.call(
            "indent",
            vec![Value::resource(&c), Value::some(Value::resource(&b))],
        );
        assert_eq!(n.parent_of(&c), Some(b.clone()));
        assert_eq!(window(&n), ["A", "B", "B1", "C"]);

        // Outdent: C leaves B, just after B and the notes it holds.
        n.call("outdent", vec![Value::resource(&c)]);
        assert_eq!(n.parent_of(&c), None);
        assert_eq!(window(&n), ["A", "B", "B1", "C"]);

        // Move up: C goes over B, and B keeps the note under it beside it.
        n.call(
            "move_up",
            vec![Value::resource(&c), Value::some(Value::resource(&b))],
        );
        assert_eq!(window(&n), ["A", "C", "B", "B1"]);

        // Move down: C goes under B and the note under B, its own branch with it.
        let under_c = n.child_of(&c);
        n.call(
            "retitle",
            vec![Value::resource(&under_c), Value::text("C1")],
        );
        n.call("move_down", vec![Value::resource(&c)]);
        assert_eq!(window(&n), ["A", "B", "B1", "C", "C1"]);

        // Move up with the branch: C and the note under it go over B's branch whole.
        n.call(
            "move_up",
            vec![Value::resource(&c), Value::some(Value::resource(&b))],
        );
        assert_eq!(window(&n), ["A", "C", "C1", "B", "B1"]);
    }

    /// The verbs take the row a view shows over the note, which may stand deeper than it: a note
    /// indented under a row of another branch goes under that branch's own at its level, never
    /// inside the row.
    #[test]
    fn indent_lands_under_the_row_it_names_branch() {
        let n = Notes::new();
        let plan = n.titled("Plan");
        let meetings = n.child_of(&plan);
        n.call(
            "retitle",
            vec![Value::resource(&meetings), Value::text("Meetings")],
        );
        let agenda = n.child_of(&meetings);
        n.call(
            "retitle",
            vec![Value::resource(&agenda), Value::text("Agenda")],
        );
        let loose = n.titled("Loose");
        // The row over it is deep in another branch: the note goes under that branch's own.
        n.call(
            "indent",
            vec![
                Value::resource(&loose),
                Value::some(Value::resource(&agenda)),
            ],
        );
        assert_eq!(n.parent_of(&loose), Some(plan));
        assert_eq!(labels(&n.window()), ["Plan", "Meetings", "Agenda", "Loose"]);
    }

    /// The note a person writes is the one the window starts a note under: the open note's row
    /// offers it, and the note it makes stands under the one open.
    #[test]
    fn the_open_notes_sub_note_is_offered_by_its_row() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let mut view = n.window();
        // The action is the note's own, so the row it stands for offers it.
        let button = of_kind(the_tree(&view), "ui.Button")
            .into_iter()
            .find(|button| {
                matches!(button.props.get("label"), Some(Value::Text(label)) if label == "New sub-note")
            })
            .unwrap_or_else(|| panic!("no sub-note control in\n{}", dump(the_tree(&view))));
        view.fire(&button.key.clone(), "press").unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            labels(&n.window()),
            ["Q3 plan", "Untitled"],
            "a note is started under the one open, with no title of its own yet"
        );
        let made = n.rt.project(&n.rt.read(&n.nb, "notes").unwrap())["count"]
            .as_i64()
            .unwrap();
        assert_eq!(made, 2, "the notebook holds the note the control made");
        let _ = plan;
    }

    /// The open note's headings stand under the tree in the window's sidebar, nested by the level
    /// they are written at, and choosing one asks the editor beside it to show that block.
    #[test]
    fn the_open_notes_headings_stand_under_the_tree() {
        let n = Notes::new();
        n.titled("Autumn plan");
        let mut view = n.window();
        // What the editor reports as the note opens, and again as its headings change: each
        // heading's own words, and the level each stands at beside them.
        let body = of_kind(view.tree(), "ui.BlockEditor")[0].key.clone();
        let lines = ["Ship the editor", "Then", "Grow the garden"];
        let levels = [2_i64, 3, 2];
        view.fire_with(
            &body,
            "headings",
            Some(&Value::List(vec![
                Value::List(lines.iter().map(|line| Value::text(*line)).collect()),
                Value::List(levels.iter().map(|level| Value::Int(*level)).collect()),
            ])),
        )
        .unwrap();
        view.poll_patches().unwrap();

        let outline = of_kind(view.tree(), "ui.NavList")
            .into_iter()
            .find(|list| {
                let title = match list.props.get("title") {
                    Some(Value::Opt(Some(held))) => Some(&**held),
                    Some(value) => Some(value),
                    None => None,
                };
                title == Some(&Value::text("Outline"))
            })
            .unwrap_or_else(|| panic!("no outline in\n{}", dump(view.tree())));
        let rows = of_kind(outline, "ui.NavItem");
        assert_eq!(
            rows.iter()
                .map(|row| text_prop(row, "label"))
                .collect::<Vec<_>>(),
            ["Ship the editor", "Then", "Grow the garden"],
            "each heading is a row of the map"
        );
        assert_eq!(
            parent_key(rows[1]),
            Some("0".to_owned()),
            "a heading stands under the shallower heading before it"
        );
        assert_eq!(parent_key(rows[2]), None, "and the next at its own level");

        // Choosing one asks the editor for that block: the place of the row, then the ask.
        view.fire(&rows[2].key.clone(), "select").unwrap();
        view.poll_patches().unwrap();
        let editor = of_kind(view.tree(), "ui.BlockEditor")[0];
        assert_eq!(
            editor.props.get("reveal"),
            Some(&Value::text("2:0")),
            "the editor is asked for the block the row names"
        );
    }

    /// The key a navigation row states as the place it stands under, when it states one.
    fn parent_key(row: &UiNode) -> Option<String> {
        match row.props.get("parent") {
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Text(key) => Some(key.clone()),
                other => panic!("a row's parent is a key, not {other:?}"),
            },
            Some(Value::Text(key)) => Some(key.clone()),
            _ => None,
        }
    }

    /// A note cannot go inside itself, nor inside anything of its own.
    #[test]
    fn a_note_never_lands_inside_itself() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let meetings = n.child_of(&plan);
        n.call(
            "retitle",
            vec![Value::resource(&meetings), Value::text("Meetings")],
        );
        let view = n.window();
        // The note it holds cannot go inside it, nor it inside that one.
        let refused = view.fire_with(
            &the_tree(&view).key.clone(),
            "into",
            Some(&Value::List(vec![
                Value::resource(&plan),
                Value::some(Value::resource(&meetings)),
            ])),
        );
        assert!(
            refused.is_err(),
            "a note moved inside the note it holds is refused"
        );
        assert_eq!(n.parent_of(&meetings), Some(plan));
    }

    /// The pinned notes stand in a shelf over the tree, and the shelf is there only while one
    /// is pinned: choosing one opens it beside the tree.
    #[test]
    fn a_pinned_note_says_so_in_its_row() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let mut view = n.window();
        let icon = |view: &ViewInstance| {
            of_kind(the_tree(view), "ui.TreeViewRow")
                .first()
                .and_then(|row| row.props.get("icon").cloned())
        };
        assert_eq!(icon(&view), Some(Value::text("file-text")));
        n.call("pin", vec![Value::resource(&plan)]);
        view.poll_patches().unwrap();
        assert_eq!(
            icon(&view),
            Some(Value::text("pin")),
            "a pinned note shows a pin in its row"
        );
        // One row per note: a window shows an item once, so its actions have one address.
        assert_eq!(labels(&view), ["Q3 plan"]);
    }

    /// The places a note's line says it stands in, the outermost first: the trail's segments,
    /// the note itself last.
    fn trail(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.BreadcrumbSegment")
            .iter()
            .map(|segment| text_prop(segment, "label"))
            .collect()
    }

    /// The moments the window shows as how long ago they were, in the order it shows them.
    fn moments(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.Time")
            .iter()
            .map(|moment| text_prop(moment, "at"))
            .collect()
    }

    /// What the window says in words, in the order it says them.
    fn said(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.Text")
            .iter()
            .map(|text| text_prop(text, "value"))
            .collect()
    }

    /// The note page's own line under the title, as the words it says: the facts, and the
    /// words of the path among them.
    fn line_says(view: &ViewInstance, word: &str) -> bool {
        said(view).iter().any(|said| said == word)
    }

    #[test]
    fn the_line_under_a_note_says_where_it_stands_and_when_it_was_written() {
        let n = Notes::at("2026-06-30T08:00:00Z");
        let plan = n.titled("Q3 plan");
        let standups = n.child_of(&plan);
        n.call(
            "retitle",
            vec![Value::resource(&standups), Value::text("Standups")],
        );
        let monday = n.child_of(&standups);
        n.call(
            "retitle",
            vec![Value::resource(&monday), Value::text("Monday")],
        );
        let mut view = n.window();
        let tree = the_tree(&view).key.clone();
        view.fire_with(&tree, "select", Some(&Value::resource(&monday)))
            .unwrap();
        view.poll_patches().unwrap();

        // Where it stands: every note over it, then the note itself, which its title says
        // again — a trail opens every place before its last.
        assert_eq!(
            trail(&view),
            ["Q3 plan", "Standups", "Monday"],
            "{}",
            dump(view.tree())
        );
        // When it was made: one moment, because nothing has written it since. The note's line
        // stands before its body and its history, whose own times come after it.
        assert_eq!(moments(&view)[..1], ["2026-06-30T08:00:00Z"]);
        assert!(line_says(&view, "Created"), "{}", dump(view.tree()));
        assert!(!line_says(&view, "Edited"), "nothing wrote it since");

        // Written into: the note holds those words, and the moment it was written is its own.
        n.set_clock("2026-06-30T08:05:00Z");
        let body = of_kind(view.tree(), "ui.BlockEditor")[0].key.clone();
        view.edit(
            &body,
            &Value::Rich(RichValue::from_text("Ada is on the sync bug.")),
        )
        .unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            moments(&view)[..2],
            ["2026-06-30T08:00:00Z", "2026-06-30T08:05:00Z"],
            "the note was made at one moment and written at another"
        );
        assert!(line_says(&view, "Edited"), "{}", dump(view.tree()));
        assert!(line_says(&view, "6 words"), "{:?}", said(&view));

        // A place over the note opens that note.
        let above = of_kind(view.tree(), "ui.BreadcrumbSegment")
            .into_iter()
            .find(|segment| text_prop(segment, "label") == "Standups")
            .unwrap_or_else(|| panic!("no Standups in\n{}", dump(view.tree())))
            .key
            .clone();
        view.fire_with(&above, "activate", None).unwrap();
        assert!(
            matches!(
                view.poll_events().as_slice(),
                [tilolang_runtime::ViewEvent::Open { target, .. }] if *target == standups
            ),
            "pressing a place opens the note it names"
        );

        // A phone keeps the one place above the note: the trail is shorter, and still opens it.
        view.set_space("", tilolang_runtime::Space::from_rem(24.4, 50.0));
        view.poll_patches().unwrap();
        assert_eq!(
            trail(&view),
            ["Standups", "Monday"],
            "a narrow page keeps the note it stands under"
        );

        // A note at the top of the tree stands nowhere: it shows no path.
        view.fire_with(&tree, "select", Some(&Value::resource(&plan)))
            .unwrap();
        view.poll_patches().unwrap();
        assert!(
            trail(&view).is_empty(),
            "a note of nothing shows no trail: {}",
            dump(view.tree())
        );
        assert!(
            line_says(&view, "Created"),
            "and still says what is known of it"
        );
    }

    /// The editor's `/` menu offers a picture and a page of the note's own, which only the app can
    /// give it: a page made from a block is a note under the open one, titled with the words that
    /// stood in the block.
    #[test]
    fn a_page_made_from_a_block_is_a_note_titled_with_its_words() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let mut view = n.window();
        let editors = of_kind(view.tree(), "ui.BlockEditor");
        let editor = editors.first().expect("the open note's body is the editor");
        assert_eq!(
            editor.props.get("slash"),
            Some(&Value::List(vec![
                Value::text("image"),
                Value::text("page"),
            ])),
            "the view offers the commands only it can carry out: {}",
            dump(view.tree())
        );
        let key = editor.key.clone();
        view.fire_with(&key, "slash_page", Some(&Value::text("Meeting notes")))
            .expect("the view makes the page");
        view.poll_patches().unwrap();

        assert_eq!(
            labels(&view),
            ["Q3 plan", "Meeting notes"],
            "the page stands under the open note: {}",
            dump(view.tree())
        );
        let made = node_titled(&view, "Meeting notes");
        assert_eq!(
            parent_of_node(made).map(|parent| parent.id_string()),
            Some(plan.id_string()),
            "it is the note's own page"
        );
    }

    #[test]
    fn the_note_page_says_where_the_note_stands() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let meetings = n.child_of(&plan);
        n.call(
            "retitle",
            vec![Value::resource(&meetings), Value::text("Meetings")],
        );
        let editor = n.rt.open_resource(&me(), &meetings, None).unwrap();
        assert_eq!(text_prop(editor.tree(), "content_kind"), "document");
        let shell = of_kind(editor.tree(), "ui.NavShell")[0];
        assert_eq!(
            text_prop(shell, "title"),
            "Meetings",
            "the bar names the note: {}",
            dump(editor.tree())
        );
        assert_eq!(
            text_prop(of_kind(editor.tree(), "ui.TextInput")[0], "placeholder"),
            "Untitled",
            "and the page edits its title"
        );
    }

    #[test]
    fn concurrent_body_edits_on_two_replicas_converge() {
        let discussion = App::compile(DISCUSSION, &[]);
        let app = App::compile(SOURCE, &[discussion.meta().clone()]);
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let rt_a = Runtime::new(store_a.clone(), SimpleAuthority::new()).unwrap();
        let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
        discussion.load(&rt_a);
        discussion.load(&rt_b);
        app.load(&rt_a);
        app.load(&rt_b);
        let nb = rt_a.create("Notebook").unwrap();
        let note = returned_ref(
            rt_a.invoke_action(
                &me(),
                (app.action("new_note").as_str(), vec![Value::resource(&nb)]),
            )
            .unwrap(),
        );
        store_a.sync_with(&mut store_b).unwrap();

        let editor = |rt: &Runtime| {
            rt.open_view(
                &me(),
                &app.view("note_editor"),
                vec![Value::resource(&note)],
            )
            .unwrap()
        };
        let (view_a, view_b) = (editor(&rt_a), editor(&rt_b));
        // The body is the editor's document; it hands over the whole text it holds.
        let body = |v: &tilolang_runtime::ViewInstance| {
            let inputs = of_kind(v.tree(), "ui.BlockEditor");
            let [input] = inputs.as_slice() else {
                panic!("the editor draws one document, the body: {inputs:#?}");
            };
            input.key.clone()
        };
        let rich = |text: &str| Value::Rich(RichValue::from_text(text));
        view_a.edit(&body(&view_a), &rich("Hello")).unwrap();
        store_a.sync_with(&mut store_b).unwrap();

        view_a.edit(&body(&view_a), &rich("Hello world")).unwrap();
        view_b.edit(&body(&view_b), &rich("Big Hello")).unwrap();
        store_a.sync_with(&mut store_b).unwrap();

        let written = |rt: &Runtime| match rt.read(&note, "body").unwrap() {
            Value::Rich(body) => body.written_text(),
            other => panic!("the body is rich text, not {other:?}"),
        };
        let (a, b) = (written(&rt_a), written(&rt_b));
        assert_eq!(a, b, "both replicas hold the same body");
        assert_eq!(a, "Big Hello world", "and it keeps both people's words");
    }
    /// An agent that knows a note by its name writes into it by that name, whatever case it
    /// types it in, and one naming no note is refused with why, the notebook unchanged.
    #[test]
    fn markdown_added_to_a_note_named_by_its_title() {
        let n = Notes::new();
        let meeting = n.titled("Meeting notes");
        n.titled("Groceries");
        n.call(
            "append_to_note",
            vec![
                Value::resource(&n.nb),
                Value::text(" meeting NOTES"),
                Value::text("- follow up with Sam"),
            ],
        );
        let written = |note: &ResourceRef| match n.rt.read(note, "body").unwrap() {
            Value::Rich(body) => body.written_text(),
            other => panic!("the body is rich text, not {other:?}"),
        };
        assert!(
            written(&meeting).contains("follow up with Sam"),
            "the Markdown lands in the note of that title: {:?}",
            written(&meeting)
        );

        let id = n.app.action("append_to_note");
        let refused = n.rt.invoke_action(
            &me(),
            (
                id.as_str(),
                vec![
                    Value::resource(&n.nb),
                    Value::text("Errands"),
                    Value::text("Bread"),
                ],
            ),
        );
        let why = format!("{refused:?}");
        assert!(why.contains("No note is titled Errands"), "{why}");
        assert_eq!(
            n.rt.project(&n.rt.read(&n.nb, "notes").unwrap())["count"]
                .as_i64()
                .unwrap(),
            2,
            "a refused write makes no note"
        );
    }

    /// What a person arrives with, dropped on the notebook or chosen from its File menu: a
    /// Markdown file, a folder of Markdown pages, a Notion export, pages with pictures, and a
    /// migration from a wiki. Each zip comes in as the notebook's tree — a page under the page
    /// whose folder it stood in — with the links between its pages kept as links between notes,
    /// the pictures it shows kept with the note, and a database exported beside a page a table
    /// at it. Importing the same pages again makes no second note.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one flow, read top to bottom")]
    fn what_a_person_arrives_with_comes_in_as_the_tree_it_was() {
        let n = Notes::new();

        // A file dropped anywhere on the notebook reaches it as the window hands it over: its
        // name, its media type and where its bytes are kept, never a path the app's process
        // cannot read. A Markdown file comes in as a note, and an archive as its pages.
        let errands = n.device.keeping(
            "Errands.md",
            "text/markdown",
            b"# Errands\n\nBread, then the post office.\n",
        );
        // The notebook's own export shape: `Plan.md`, a page under its folder, and that page's
        // own with a link back up to the one it stands in.
        let folder = n.device.archive(
            "Notes.zip",
            &[
                (
                    "Plan.md",
                    "# Plan\n\nShip the offline editor. See [the standups](Plan/Standups.md).\n"
                        .as_bytes(),
                ),
                (
                    "Plan/Standups.md",
                    "# Standups\n\nTen minutes, every morning.\n\n[Monday](Standups/Monday.md)\n"
                        .as_bytes(),
                ),
                (
                    "Plan/Standups/Monday.md",
                    "# Monday\n\nAda is on the sync bug. Back to [the plan](../../Plan.md).\n"
                        .as_bytes(),
                ),
            ],
        );
        let view = n.window();
        let shell = of_kind(view.tree(), "ui.NavShell")
            .into_iter()
            .next()
            .expect("the notebook is a shell");
        assert!(
            shell.events.contains_key("drop"),
            "the notebook takes a file dropped anywhere on it"
        );
        let shell = shell.key.clone();
        view.fire_with(
            &shell,
            "drop",
            Some(&Value::List(vec![Value::file(errands)])),
        )
        .expect("the notebook takes the dropped file");
        assert_eq!(
            n.titles(),
            ["Errands"],
            "the Markdown file came in as a note"
        );
        view.fire_with(
            &shell,
            "drop",
            Some(&Value::List(vec![Value::file(folder.clone())])),
        )
        .expect("the notebook takes the dropped archive");
        assert_eq!(
            n.titles(),
            ["Errands", "Plan", "Standups", "Monday"],
            "the archive's pages came in beside the note the file made, one note per page"
        );
        let plan = n.note_titled("Plan");
        let standups = n.note_titled("Standups");
        let monday = n.note_titled("Monday");
        assert_eq!(
            n.parent_of(&standups),
            Some(ResourceRef::clone(&plan)),
            "a page stands under the page whose folder it was written in"
        );
        assert_eq!(n.parent_of(&monday), Some(ResourceRef::clone(&standups)));
        assert_eq!(n.parent_of(&plan), None);
        // A link to a page of the archive is the reference to the note that page made, wherever
        // the reader of the file is: the note opens, it is not a web address.
        assert_eq!(
            n.linked_ids(&plan),
            vec![standups.id_string()],
            "the link to the standups names their note"
        );
        assert_eq!(n.linked_ids(&standups), vec![monday.id_string()]);
        assert_eq!(
            n.linked_ids(&monday),
            vec![plan.id_string()],
            "a link up out of a folder names the page it left"
        );
        // The heading that named the page is its title, not its body's first line again.
        let body = n.body_of(&plan).written_text();
        assert!(
            !body.contains("# Plan"),
            "the title's heading is not said twice: {body}"
        );

        // A Notion export: the pages it wrote, the database it wrote beside one as a CSV, and a
        // page it never titled.
        n.import(&n.device.archive(
            "Export.zip",
            &[
                ("Garden.md", "# Garden\n\nGrow the garden.\n".as_bytes()),
                (
                    "Garden Tasks.csv",
                    "Task,Owner\nShip the editor,Ada\nGrow the garden,\n".as_bytes(),
                ),
                (
                    "Untitled 1f2e3d4c.md",
                    "# Notes from the call\n\nThree people, two decisions.\n".as_bytes(),
                ),
                ("__MACOSX/Garden.md", "not a page".as_bytes()),
            ],
        ));
        assert_eq!(
            n.titles(),
            [
                "Errands",
                "Plan",
                "Standups",
                "Monday",
                "Garden",
                "Notes from the call"
            ],
            "a page the export never titled takes the heading it says, and the archive's own \
             bookkeeping is no page"
        );
        let body = n.body_of(&n.note_titled("Garden")).written_text();
        assert!(
            body.contains("| Task | Owner |") && body.contains("| Ship the editor | Ada |"),
            "the database beside the page is a table at it: {body}"
        );
        assert!(
            body.contains("| Grow the garden |  |"),
            "a row's empty field is an empty cell: {body}"
        );

        // A picture a page shows is kept with the note, and named: an export writes it beside
        // the note under the name it came in as.
        let pixels = png();
        n.import(&n.device.archive(
            "Trip.zip",
            &[
                (
                    "Trip.md",
                    "# Trip\n\n![a fox](Trip/images/fox.png)\n".as_bytes(),
                ),
                ("Trip/images/fox.png", pixels.as_slice()),
            ],
        ));
        let trip = n.note_titled("Trip");
        let images = n.images_of(&trip);
        assert_eq!(images.len(), 1, "the note keeps the picture it shows");
        let (Value::Text(address), Value::Text(name)) = (&images[0].0, &images[0].1) else {
            panic!("a picture is kept by its address and its name: {images:?}");
        };
        assert!(address.starts_with("tilo-blob://"), "{address}");
        assert_eq!(name, "fox.png");
        assert!(
            n.body_of(&trip).written_text().contains(address),
            "the body shows the picture from where it is kept"
        );

        // An import from another service says where each page came from in its own file beside
        // the pages: a page keeps the title that service gave it, who wrote it, its address
        // there, the page it stood under and its dates.
        let record = |path: &str, title: &str, page: &str, above: &str| {
            format!(
                "{path}\t{title}\tAda\tOutline\t{page}\t{above}\t2025-08-01T10:00:00Z\t\
                 2025-09-01T10:00:00Z\n"
            )
        };
        let said = format!(
            "tilo-import 1\n{}{}",
            record(
                "Q3 plan.md",
                "Q3 plan",
                "https://outline.example/doc/top",
                ""
            ),
            record(
                "Q3 plan/Ideas.md",
                "Ideas to ship",
                "https://outline.example/doc/ideas",
                "https://outline.example/doc/top"
            ),
        );
        n.import(
            &n.device.archive(
                "Outline pages.zip",
                &[
                    ("tilo-import.tsv", said.as_bytes()),
                    (
                        "Q3 plan.md",
                        "# Q3 plan\n\nShip the offline editor.\n".as_bytes(),
                    ),
                    (
                        "Q3 plan/Ideas.md",
                        "Three ideas, and no heading of its own.\n\n\
                     ![a fox](Ideas/images/fox.png)\n"
                            .as_bytes(),
                    ),
                    ("Q3 plan/Ideas/images/fox.png", pixels.as_slice()),
                ],
            ),
        );
        let arrived = [
            "Errands",
            "Plan",
            "Standups",
            "Monday",
            "Garden",
            "Notes from the call",
            "Trip",
            "Q3 plan",
            "Ideas to ship",
        ];
        assert_eq!(n.titles(), arrived, "a note a page");
        let q3 = n.note_titled("Q3 plan");
        let ideas = n.note_titled("Ideas to ship");
        assert_eq!(
            n.parent_of(&ideas),
            Some(ResourceRef::clone(&q3)),
            "the page stands under the page it stood under there, by that page's address"
        );
        assert_eq!(n.said_of(&ideas, "by"), "Ada");
        assert_eq!(n.said_of(&ideas, "service"), "Outline");
        assert_eq!(
            n.said_of(&ideas, "page"),
            "https://outline.example/doc/ideas"
        );
        assert_eq!(n.said_of(&ideas, "created"), "2025-08-01T10:00:00Z");
        assert_eq!(n.said_of(&ideas, "updated"), "2025-09-01T10:00:00Z");
        assert_eq!(
            n.title_of(&ideas),
            "Ideas to ship",
            "the service's title, not its file's name"
        );
        let body = n.body_of(&ideas).written_text();
        assert!(
            body.contains("Three ideas, and no heading of its own."),
            "{body}"
        );
        let images = n.images_of(&ideas);
        assert_eq!(images.len(), 1, "the picture the page shows: {images:?}");
        assert_eq!(images[0].1, Value::text("fox.png"));

        // Importing the page again by its address there — however its words changed — opens the
        // note it made rather than making a second one, and the note is the person's now.
        n.import(&n.device.archive(
            "Outline pages.zip",
            &[
                ("tilo-import.tsv", said.as_bytes()),
                (
                    "Q3 plan/Ideas.md",
                    "Rewritten at the source since.\n".as_bytes(),
                ),
            ],
        ));
        assert_eq!(n.titles(), arrived, "the page already made its note");
        assert!(
            n.body_of(&ideas).written_text().contains("Three ideas"),
            "the note is the person's now, as they left it"
        );

        // The same archive imported again keeps one note of each page: a note remembers the
        // file it came from.
        n.import(&folder);
        assert_eq!(n.titles(), arrived, "the pages already made their notes");
    }

    /// One page at a time: a file the person imports themselves, a page `tilo migrate` brings
    /// from a service, and a file a caller outside the window names by the content it put. Each
    /// is a note of the notebook, drawn in its tree and named by the document's first heading,
    /// standing under the note of its folder; a service's page keeps whose words it is and where
    /// it is read there, and importing it again — its words changed on the service since — opens
    /// the note it made rather than making a second one.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one flow, read top to bottom")]
    fn a_page_comes_in_as_a_note_under_its_folder() {
        let n = Notes::new();
        n.titled("Trip to Lisbon");
        let imported = n.import_file("Lisbon.md", "# Lisbon in May\n\nA week of trams and tiles.");
        let mut view = n.window();
        assert_eq!(
            labels(&view),
            ["Trip to Lisbon", "Lisbon in May"],
            "the imported page is a row of the notebook's tree"
        );
        // The window shows what the import made: its view answers `on show` for the note, and
        // the row it opens is the one chosen.
        assert!(
            view.show(&imported).unwrap(),
            "the notebook shows the note the import made"
        );
        view.poll_patches().unwrap();
        assert_eq!(
            the_tree(&view).props.get("selection"),
            Some(&Value::List(vec![Value::resource(&imported)])),
            "the row of the note the import made is the one chosen"
        );

        let plan = n.import_page(
            "Q3 plan.md",
            "# Q3 plan\n\nShip the offline editor.",
            "Ada",
            "Outline",
            "https://outline.example/doc/q3-plan",
        );
        assert_eq!(n.said_of(&plan, "title"), "Q3 plan");
        assert_eq!(n.said_of(&plan, "by"), "Ada");
        assert_eq!(n.said_of(&plan, "service"), "Outline");
        assert_eq!(
            n.said_of(&plan, "page"),
            "https://outline.example/doc/q3-plan"
        );
        assert_eq!(n.said_of(&plan, "created"), "2025-08-01T10:00:00Z");
        assert_eq!(n.said_of(&plan, "updated"), "2025-09-01T10:00:00Z");
        assert_eq!(n.said_of(&plan, "origin"), "Q3 plan.md");
        assert_eq!(n.parent_of(&plan), None, "a top page stands at the top");

        let ideas = n.import_page(
            "Q3 plan/Ideas.md",
            "# Ideas\n\n- a table editor",
            "Grace",
            "Outline",
            "https://outline.example/doc/ideas",
        );
        assert_eq!(
            n.parent_of(&ideas).as_ref(),
            Some(&plan),
            "a page of that page's folder stands under it"
        );
        assert_eq!(
            n.said_of(&ideas, "by"),
            "Grace",
            "each page keeps its own author"
        );

        let again = n.import_page(
            "Q3 plan.md",
            "# Q3 plan\n\nShip the offline editor, then sharing.",
            "Ada",
            "Outline",
            "https://outline.example/doc/q3-plan",
        );
        assert_eq!(again, plan, "a page imported again is the note it made");
        assert_eq!(
            n.titles(),
            ["Trip to Lisbon", "Lisbon in May", "Q3 plan", "Ideas"],
            "and the notebook holds no second note of it"
        );

        // A file the person imports themselves stands under its folder's note too, and says
        // nothing of a service it did not come from.
        let reading = n.import_file("Reading list.md", "# Reading list\n\n- one book a month");
        let books = n.import_file("Reading list/Books.md", "# Books\n\n- the one about tides");
        assert_eq!(
            n.parent_of(&books).as_ref(),
            Some(&reading),
            "a file of that folder's own stands under it"
        );
        assert_eq!(n.said_of(&books, "service"), "", "no service wrote it");
        assert_eq!(n.said_of(&books, "origin"), "Reading list/Books.md");

        // What a caller outside the window gives a `File` parameter: the content it put into the
        // workspace, as `tilo_put_finish` answered it, and the file's own name and media type.
        // The action imports it as the person's own chooser would.
        let put = n.device.keeping(
            "Standup.md",
            "text/markdown",
            b"# Imported\n\nWords from a file.\n",
        );
        let named = serde_json::json!({"file": {
            "content": tilo_actions::content_resource(&put.hash).expect("a reference"),
            "name": put.name,
            "media": put.media,
        }});
        let file = tilolang_runtime::value::from_json(&named).expect("a reference names a file");
        let made = returned_ref(n.call("import_markdown", vec![Value::resource(&n.nb), file]));
        assert_eq!(n.title_of(&made), "Imported");
        assert!(
            n.body_of(&made)
                .written_text()
                .contains("Words from a file."),
            "the file's words are the note's"
        );
        assert_eq!(
            n.said_of(&made, "origin"),
            "Standup.md",
            "the note keeps the name the file came in as"
        );

        // A reference this workspace's store holds no content for is no file: the action still
        // runs, and what it answers says why the content it was given could not be read. Naming
        // a reference never reaches bytes the store does not hold.
        let named = serde_json::json!({"file": {
            "content": tilo_actions::content_resource(&"0".repeat(64)).expect("a reference"),
            "name": "Gone.md",
        }});
        let file = tilolang_runtime::value::from_json(&named).expect("a reference names a file");
        let made = returned_ref(n.call("import_markdown", vec![Value::resource(&n.nb), file]));
        let body = n.body_of(&made).written_text();
        assert!(
            body.contains("the content of `Gone.md` is not on this device"),
            "the note says why the file was no words: {body}"
        );
    }

    /// A picture the editor put into a note's body is named by the note, so an export writes it
    /// beside the note under the name it came in as.
    #[test]
    fn a_picture_the_editor_puts_in_is_named_by_the_note() {
        let n = Notes::new();
        let note = returned_ref(n.call(
            "new_note",
            vec![
                Value::resource(&n.nb),
                Value::text("Plan"),
                Value::text("Ship the offline editor."),
            ],
        ));
        let view = n.window();
        let editor = of_kind(view.tree(), "ui.BlockEditor")
            .into_iter()
            .next()
            .expect("the open note's body is the editor")
            .key
            .clone();
        // What the editor says of a picture it wrote in at the caret: its name, then where its
        // bytes were kept.
        view.fire_with(
            &editor,
            "image",
            Some(&Value::List(vec![
                Value::text("Pasted image.png"),
                Value::text("tilo-blob://pixels"),
            ])),
        )
        .expect("the view takes the picture");
        assert_eq!(
            n.images_of(&note),
            vec![(
                Value::text("tilo-blob://pixels"),
                Value::text("Pasted image.png")
            )],
            "the note keeps the picture's name beside the address of its bytes"
        );
    }
}
