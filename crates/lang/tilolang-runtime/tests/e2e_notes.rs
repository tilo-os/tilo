//! The notes app (`apps/tl/notes/app.tl`) end to end: a notebook whose notes are a tree.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::rich::RichValue;
    use tilolang_runtime::{
        AmStore, EditOutcome, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
        ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/notes/app.tl");
    const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");

    struct Notes {
        rt: Runtime,
        app: App,
        nb: ResourceRef,
    }

    impl Notes {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let discussion = App::compile(DISCUSSION, &[]);
            discussion.load(&rt);
            let app = App::compile(SOURCE, &[discussion.meta().clone()]);
            app.load(&rt);
            let nb = rt.create("Notebook").unwrap();
            Self { rt, app, nb }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn new_note(&self) -> ResourceRef {
            returned_ref(self.call("new_note", vec![Value::resource(&self.nb)]))
        }

        fn child_of(&self, parent: &ResourceRef) -> ResourceRef {
            returned_ref(self.call(
                "new_child",
                vec![Value::resource(&self.nb), Value::resource(parent)],
            ))
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

    /// Fire the value-carrying `event` of the tree node of the note titled `title`.
    fn move_on(view: &ViewInstance, title: &str, event: &str, moved: &ResourceRef) {
        let key = node_titled(view, title).key.clone();
        view.fire_with(&key, event, Some(&Value::resource(moved)))
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
    /// it — no list of them between. The sidebar's header carries the "+" that starts one.
    #[test]
    fn the_notebook_is_the_tree_beside_the_note_it_opens() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        n.call("pin", vec![Value::resource(&plan)]);
        let mut view = n.window();
        assert_eq!(
            text_prop(of_kind(view.tree(), "ui.NavShell")[0], "content_kind"),
            "document",
            "the window is a document with a sidebar: {}",
            dump(view.tree())
        );
        assert_eq!(
            text_prop(of_kind(view.tree(), "ui.NavShell")[0], "sidebar_title"),
            "Notes"
        );
        assert_eq!(
            labels(&view),
            ["Q3 plan"],
            "the tree holds the notes: {}",
            dump(view.tree())
        );
        // The window's "+", in the sidebar's header.
        let head = of_kind(view.tree(), "ui.Button")
            .into_iter()
            .find(|button| text_prop(button, "label") == "New note")
            .unwrap_or_else(|| panic!("no New note button in\n{}", dump(view.tree())));
        assert!(
            of_kind(view.tree(), "ui.NavShell")[0]
                .children
                .iter()
                .flat_map(|child| of_kind(child, "ui.Button"))
                .any(|button| button.key == head.key),
            "it stands in the sidebar's header, not in the bar"
        );
        // The note the window opens is the first of the tree, drawn beside it.
        let body = of_kind(view.tree(), "ui.BlockEditor");
        assert_eq!(body.len(), 1, "the open note's body stands beside the tree");
        assert_eq!(
            text_prop(of_kind(view.tree(), "ui.TextInput")[0], "placeholder"),
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

    /// Dropping a note on a row makes it a note of that one: the tree indents it under the row
    /// it landed on, and the store holds it there.
    #[test]
    fn a_note_dropped_on_a_row_stands_inside_it() {
        let n = Notes::new();
        let plan = n.titled("Q3 plan");
        let meetings = n.titled("Meetings");
        let mut view = n.window();
        move_on(&view, "Meetings", "into", &plan);
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

        // Dropped before a row, it takes that row's place among its own: a note of it keeps
        // the place it has in the order.
        n.titled("Notes");
        view.poll_patches().unwrap();
        move_on(&view, "Notes", "drop", &meetings);
        assert_eq!(n.parent_of(&meetings), None);
        view.poll_patches().unwrap();
        assert_eq!(
            labels(&view),
            ["Q3 plan", "Meetings", "Notes"],
            "the tree keeps the order the store does"
        );
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
            &node_titled(&view, "Meetings").key.clone(),
            "into",
            Some(&Value::resource(&plan)),
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
}
