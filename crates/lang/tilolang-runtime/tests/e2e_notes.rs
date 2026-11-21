//! The notes app (`apps/tl/notes/app.tl`) end to end: a notebook whose notes are a tree.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::rich::RichValue;
    use tilolang_runtime::{
        AmStore, EditOutcome, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
        ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/notes/app.tl");
    const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");

    /// The device the notebook's files are read from: its content store, of what a test kept
    /// there. A file's hash is its bytes, as a content store addresses them.
    #[derive(Clone, Default)]
    struct Device(std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, Vec<u8>>>>);

    impl Device {
        /// The file `name` of `text`, kept in this device's store.
        fn keeping(
            &self,
            name: &str,
            media: &str,
            text: &str,
        ) -> tilolang_runtime::blob::FileValue {
            let hash = digest_of(text.as_bytes());
            self.0
                .borrow_mut()
                .insert(hash.clone(), text.as_bytes().to_vec());
            tilolang_runtime::blob::FileValue::new(
                name,
                media,
                i64::try_from(text.len()).unwrap_or(i64::MAX),
                hash,
            )
        }
    }

    impl tilolang_runtime::blob::BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(
            &self,
            _request: &tilolang_runtime::blob::FileRequest,
        ) -> Result<Option<tilolang_runtime::blob::FileValue>, String> {
            Ok(None)
        }

        fn save(
            &self,
            _request: &tilolang_runtime::blob::SaveRequest,
            _content: &[u8],
        ) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(
            &self,
            name: &str,
            media: &str,
            bytes: &[u8],
        ) -> Result<tilolang_runtime::blob::FileValue, String> {
            let text = String::from_utf8_lossy(bytes).into_owned();
            Ok(self.keeping(name, media, &text))
        }
    }

    /// The address a content store names `bytes` by: the same bytes are one file, whatever
    /// name they came in as.
    fn digest_of(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut hex = String::with_capacity(17);
        let _ = write!(hex, "h{digest:016x}");
        hex
    }

    struct Notes {
        rt: Runtime,
        app: App,
        nb: ResourceRef,
        /// The device whose files the notebook reads, as a person's own store holds them.
        device: Device,
        /// What the workspace's clock says: when a note is made, and when it is written since.
        clock: std::rc::Rc<std::cell::RefCell<String>>,
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
            let clock = std::rc::Rc::new(std::cell::RefCell::new(now.to_owned()));
            let reading = std::rc::Rc::clone(&clock);
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
            let file = Value::file(self.device.keeping(name, "text/markdown", text));
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
            let file = Value::file(self.device.keeping(name, "text/markdown", text));
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
    /// offers it — as `Ctrl+Shift+N` does — and the note it makes stands under the one open.
    #[test]
    fn the_open_notes_sub_note_is_a_keystroke_away() {
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
        let key = n
            .app
            .meta()
            .actions
            .iter()
            .find(|action| action.id == n.app.action("new_child"))
            .and_then(|action| action.key.clone());
        assert_eq!(
            key.as_deref(),
            Some("Ctrl+Shift+n"),
            "a note under the open one is a keystroke away"
        );
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

    /// A page the person imports is a note of the notebook, drawn in its tree at the top and
    /// named by the document's first heading; the window shows it when the host shows the note
    /// the import made, as it shows what a press made.
    #[test]
    fn an_imported_page_is_a_note_the_window_shows() {
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

    /// A service's pages come over as the tree they were: a page whose file's path stands in
    /// another's folder lands under the note of that page, each page keeps whose words it is and
    /// where it is read there, and importing a page again — its words changed on the service
    /// since — opens the note it made rather than making a second one.
    #[test]
    fn a_services_pages_come_over_as_the_tree_they_were() {
        let n = Notes::new();
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
            n.rt.project(&n.rt.read(&n.nb, "notes").unwrap())["count"]
                .as_i64()
                .unwrap(),
            2,
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
    }
}
