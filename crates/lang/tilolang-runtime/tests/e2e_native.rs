//! The native vocabulary run: a list's items and rows, its sort and filter as view state with
//! counts per filter, a row view, and the choice and facts a detail view shows.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, UiNode, Value};

    const SOURCE: &str = r#"app so.tilo.notes "Notes" (icon: .bookmark)

type Note {
    title: Text (title)
    pinned: Bool = false
}

type Notebook {
    notes: Collection<Note>
}

enum NoteFilter {
    /// All notes
    all
    pinned
}

enum NoteOrder {
    created
    title
}

root Notebook

default view notebook for Notebook

row view note_row for Note

primary action add(nb: Notebook, title: Text, pinned: Bool) -> Note (icon: .plus) {
    return nb.notes.insert(Note(title: title, pinned: pinned))
}

fn listed(nb: Notebook, filter: NoteFilter) -> List<Note> {
    return match filter {
        .all => nb.notes.filter(n -> true),
        .pinned => nb.notes.filter(n -> n.pinned),
    }
}

view note_row(n: Note) {
    row(n.title, mark: if n.pinned { .pin })
}

view notebook(nb: Notebook) {
    window order: NoteOrder = .created
    window filter: NoteFilter = .all
    list(listed(nb, filter), sort: order, filter: filter, empty: "No notes yet") { n ->
        row(n.title, checkbox: n.pinned)
    }
}

view rows(nb: Notebook) {
    list(nb.notes)
}

view detail(n: Note) {
    lens shown: NoteFilter = .pinned
    document {
        choice(bind: shown)
        facts {
            "Title": n.title
            "Pinned": n.pinned
        }
    }
}
"#;

    fn setup() -> (Runtime, App, ResourceRef) {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let nb = rt.create("Notebook").unwrap();
        let add = app.action("add");
        for (title, pinned) in [("Q3 plan", true), ("Groceries", false), ("Ideas", false)] {
            rt.invoke_action(
                &me(),
                (
                    add.as_str(),
                    vec![
                        Value::resource(&nb),
                        Value::text(title),
                        Value::Bool(pinned),
                    ],
                ),
            )
            .unwrap();
        }
        (rt, app, nb)
    }

    fn list(tree: &UiNode) -> &UiNode {
        of_kind(tree, "list")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no list in\n{}", dump(tree)))
    }

    fn titles(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "list_row")
            .into_iter()
            .map(|r| text_prop(r, "title"))
            .collect()
    }

    #[test]
    fn a_list_holds_one_item_per_resource_keyed_by_it_with_its_row() {
        let (rt, app, nb) = setup();
        let view = rt
            .open_view(&me(), &app.view("notebook"), vec![Value::resource(&nb)])
            .unwrap();
        let tree = view.tree();
        let l = list(tree);
        assert_eq!(l.key, tree.key, "the frame is the view's root");
        assert_eq!(
            l.props["item_type"],
            Value::text(app.meta().types[0].id.clone())
        );
        assert_eq!(text_prop(l, "empty"), "No notes yet");
        assert_eq!(l.children.len(), 3, "{}", dump(tree));
        for item in &l.children {
            assert_eq!(item.kind, "item");
            let Value::Ref(r) = &item.props["item"] else {
                panic!("an item carries its resource: {}", dump(item));
            };
            assert_eq!(item.key, format!("{}/{}", l.key, r.target.id_string()));
            assert_eq!(item.children.len(), 1);
            assert_eq!(item.children[0].kind, "list_row");
        }
        assert_eq!(titles(tree), ["Q3 plan", "Groceries", "Ideas"]);
        let row = &l.children[0].children[0];
        assert_eq!(view.bound_value(&row.key), Some(Value::Bool(true)));
    }

    #[test]
    fn sort_and_filter_are_view_state_with_a_count_per_filter() {
        let (rt, app, nb) = setup();
        let mut view = rt
            .open_view(&me(), &app.view("notebook"), vec![Value::resource(&nb)])
            .unwrap();
        let l = list(view.tree());
        assert_eq!(text_prop(l, "sort.selected"), "created");
        assert_eq!(text_prop(l, "filter.selected"), "all");
        assert_eq!(l.props["filter.count.all"], Value::Int(3));
        assert_eq!(l.props["filter.count.pinned"], Value::Int(1));
        for event in ["sort.created", "sort.title", "filter.all", "filter.pinned"] {
            assert!(l.events.contains_key(event), "{event}: {}", dump(l));
        }
        let key = l.key.clone();
        view.fire(&key, "filter.pinned").unwrap();
        view.poll_patches().unwrap();
        let l = list(view.tree());
        assert_eq!(text_prop(l, "filter.selected"), "pinned");
        assert_eq!(titles(view.tree()), ["Q3 plan"]);
        assert_eq!(
            (
                &l.props["filter.count.all"],
                &l.props["filter.count.pinned"]
            ),
            (&Value::Int(3), &Value::Int(1)),
            "the counts do not depend on the filter shown"
        );
        view.fire(&key, "sort.title").unwrap();
        view.poll_patches().unwrap();
        assert_eq!(text_prop(list(view.tree()), "sort.selected"), "title");
    }

    #[test]
    fn a_list_without_an_item_block_shows_the_types_row_view() {
        let (rt, app, nb) = setup();
        let view = rt
            .open_view(&me(), &app.view("rows"), vec![Value::resource(&nb)])
            .unwrap();
        let l = list(view.tree());
        assert_eq!(l.children.len(), 3);
        let rows = of_kind(&l.children[0], "list_row");
        assert_eq!(rows.len(), 1, "{}", dump(l));
        assert_eq!(text_prop(rows[0], "title"), "Q3 plan");
        assert_eq!(text_prop(rows[0], "mark"), "pin");
        assert!(
            !of_kind(&l.children[1], "list_row")[0]
                .props
                .contains_key("mark")
        );
    }

    #[test]
    fn a_choice_picks_view_state_and_facts_show_their_values() {
        let (rt, app, nb) = setup();
        let Value::List(notes) = rt.read(&nb, "notes").unwrap() else {
            panic!("notes");
        };
        let Value::Ref(note) = &notes[0] else {
            panic!("a note");
        };
        let mut view = rt
            .open_view(
                &me(),
                &app.view("detail"),
                vec![Value::resource(&note.target)],
            )
            .unwrap();
        let choice = of_kind(view.tree(), "choice")[0].clone();
        assert_eq!(text_prop(&choice, "selected"), "pinned");
        assert_eq!(text_prop(&choice, "enum"), app.meta().enums[0].id.clone());
        view.fire(&choice.key, "pick.all").unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            text_prop(of_kind(view.tree(), "choice")[0], "selected"),
            "all"
        );
        let facts = of_kind(view.tree(), "fact");
        let pairs: Vec<(String, Value)> = facts
            .iter()
            .map(|f| (text_prop(f, "label"), f.props["value"].clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("Title".to_string(), Value::text("Q3 plan")),
                ("Pinned".to_string(), Value::Int(1)),
            ]
        );
    }
}
