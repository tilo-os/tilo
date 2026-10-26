//! Every view call is a boundary: a change re-runs the views that read it and no others, and
//! the rows of a `list` render when the host shows them.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Patch, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
        ViewInstance,
    };

    const SOURCE: &str = r#"app so.tilo.rows "Rows"

type Item {
    title: Text
    done: Bool = false
}

type Bag {
    name: Text
    items: Collection<Item>
}

action add(bag: Bag, title: Text) -> Item {
    return bag.items.insert(Item(title: title))
}

action retitle(item: Item, title: Text) {
    item.title = title
}

action move_to(bag: Bag, item: Item, index: Int) {
    bag.items.move(item, index)
}

action remove(bag: Bag, item: Item) {
    bag.items.remove(item)
}

action rename(bag: Bag, name: Text) {
    bag.name = name
}

view item_row(item: Item) {
    row {
        text(item.title)
        checkbox("Done", bind: item.done)
    }
}

view eager(bag: Bag) {
    section(bag.name) {
        for item in bag.items {
            item_row(item)
        }
    }
}

view inline(bag: Bag) {
    section(bag.name) {
        for item in bag.items {
            row {
                text(item.title)
                checkbox("Done", bind: item.done)
            }
        }
    }
}

view lazy(bag: Bag) {
    list(bag.items) { item ->
        row(item.title, checkbox: item.done)
    }
}

view oldest(bag: Bag) {
    section(bag.name) {
        text(match bag.items.first() {
            .some(item) => item.title,
            .none => "empty",
        })
    }
}

view newest(bag: Bag) {
    section(bag.name) {
        text(match bag.items.last() {
            .some(item) => item.title,
            .none => "empty",
        })
    }
}

view count(bag: Bag) {
    section(bag.name) {
        text("{bag.items.len()}")
    }
}
"#;

    struct Rows {
        rt: Runtime,
        app: App,
        bag: ResourceRef,
        items: Vec<ResourceRef>,
    }

    impl Rows {
        fn new(n: usize) -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let bag = rt.create("Bag").unwrap();
            let mut rows = Self {
                rt,
                app,
                bag,
                items: Vec::new(),
            };
            for i in 0..n {
                let out = rows.call(
                    "add",
                    vec![Value::resource(&rows.bag), Value::text(format!("item {i}"))],
                );
                rows.items.push(returned_ref(out));
            }
            rows
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn view(&self, name: &str) -> ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view(name),
                    vec![Value::resource(&self.bag)],
                )
                .unwrap()
        }

        fn retitle(&self, i: usize, title: &str) {
            self.call(
                "retitle",
                vec![Value::resource(&self.items[i]), Value::text(title)],
            );
        }
    }

    fn texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    /// The titles of the list rows under `tree`.
    fn titles(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "list_row")
            .iter()
            .map(|t| text_prop(t, "title"))
            .collect()
    }

    fn list_key(view: &ViewInstance) -> String {
        of_kind(view.tree(), "list")[0].key.clone()
    }

    #[test]
    fn one_items_change_re_runs_only_its_row() {
        let rows = Rows::new(50);
        let mut view = rows.view("eager");
        let opened = view.stats();
        assert_eq!(opened.boundaries, 51, "the view and one boundary per row");
        assert_eq!(opened.renders, 51);

        rows.retitle(17, "seventeen");
        let patches = view.poll_patches().unwrap();
        assert_eq!(
            view.stats().renders - opened.renders,
            1,
            "only the retitled item's row ran"
        );
        assert!(
            matches!(patches.as_slice(), [Patch::SetProp { name, .. }] if name == "value"),
            "{patches:?}"
        );
        assert_eq!(texts(view.tree())[17], "seventeen");
    }

    #[test]
    fn an_inline_loop_body_is_a_boundary_per_item() {
        // No view factored out: the loop's body is a row of its own all the same.
        let rows = Rows::new(50);
        let mut view = rows.view("inline");
        let opened = view.stats();
        assert_eq!(opened.boundaries, 51, "the view and one boundary per row");
        rows.retitle(17, "seventeen");
        let patches = view.poll_patches().unwrap();
        assert_eq!(
            view.stats().renders - opened.renders,
            1,
            "only that row ran"
        );
        assert!(
            matches!(patches.as_slice(), [Patch::SetProp { name, .. }] if name == "value"),
            "{patches:?}"
        );
        assert_eq!(texts(view.tree())[17], "seventeen");
    }

    #[test]
    fn renaming_the_bag_re_runs_the_bag_view_but_no_row() {
        let rows = Rows::new(20);
        let mut view = rows.view("eager");
        let opened = view.stats();
        rows.call(
            "rename",
            vec![Value::resource(&rows.bag), Value::text("Groceries")],
        );
        let patches = view.poll_patches().unwrap();
        assert_eq!(
            view.stats().renders - opened.renders,
            1,
            "the rows were kept"
        );
        assert_eq!(patches.len(), 1, "{patches:?}");
        assert_eq!(
            text_prop(of_kind(view.tree(), "section")[0], "title"),
            "Groceries"
        );
    }

    #[test]
    fn moves_inserts_and_removes_keep_the_other_rows() {
        let rows = Rows::new(6);
        let mut view = rows.view("eager");
        let opened = view.stats();
        rows.call(
            "move_to",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[4]),
                Value::Int(1),
            ],
        );
        let patches = view.poll_patches().unwrap();
        assert!(
            patches.iter().all(|p| matches!(p, Patch::Move { .. })),
            "{patches:?}"
        );
        assert_eq!(
            texts(view.tree()),
            ["item 0", "item 4", "item 1", "item 2", "item 3", "item 5"]
        );
        rows.call(
            "remove",
            vec![Value::resource(&rows.bag), Value::resource(&rows.items[2])],
        );
        let removed = view.poll_patches().unwrap();
        assert!(
            matches!(removed.as_slice(), [Patch::Remove { .. }]),
            "{removed:?}"
        );
        rows.call("add", vec![Value::resource(&rows.bag), Value::text("new")]);
        let added = view.poll_patches().unwrap();
        assert!(
            matches!(added.as_slice(), [Patch::Insert { .. }]),
            "{added:?}"
        );
        assert_eq!(
            texts(view.tree()),
            ["item 0", "item 4", "item 1", "item 3", "item 5", "new"]
        );
        // The bag view ran for each of the three; the one new row ran once; no other row ran.
        assert_eq!(view.stats().renders - opened.renders, 4);
    }

    #[test]
    fn a_lists_rows_render_when_shown_and_stop_when_hidden() {
        let rows = Rows::new(1_000);
        let mut view = rows.view("lazy");
        let list = list_key(&view);
        assert_eq!(view.list_len(&list), Some(1_000));
        let opened = view.stats();
        assert_eq!(opened.renders, 1, "no row ran before it was shown");
        assert_eq!(opened.unrendered, 1_000);

        // The host shows what fits.
        for i in 0..20 {
            let row = view.row(&list, i).unwrap().unwrap();
            assert_eq!(titles(row), [format!("item {i}")]);
        }
        assert_eq!(view.stats().renders, 21);

        // A shown row follows its item.
        rows.retitle(3, "three");
        let patches = view.poll_patches().unwrap();
        assert_eq!(patches.len(), 1, "{patches:?}");
        assert_eq!(view.stats().renders, 22);
        // An unshown one does not run.
        rows.retitle(500, "five hundred");
        assert!(view.poll_patches().unwrap().is_empty());
        assert_eq!(view.stats().renders, 22);
        assert_eq!(
            titles(view.row(&list, 500).unwrap().unwrap()),
            ["five hundred"]
        );

        // Scrolled away, a row is forgotten: it no longer runs, and renders again when shown.
        let hidden = view.hide_rows(&list, 10..20);
        assert!(hidden.iter().all(|p| matches!(p, Patch::Remove { .. })));
        let before = view.stats().renders;
        rows.retitle(3, "drei");
        assert!(view.poll_patches().unwrap().is_empty());
        assert_eq!(view.stats().renders, before);
        assert_eq!(titles(view.row(&list, 3).unwrap().unwrap()), ["drei"]);
    }

    #[test]
    fn a_host_that_draws_everything_turns_lazy_rows_off() {
        let rows = Rows::new(5);
        let mut view = rows.view("lazy");
        view.set_lazy_rows(false).unwrap();
        assert_eq!(view.stats().unrendered, 0);
        assert_eq!(titles(view.tree()).len(), 5);
        rows.retitle(0, "first");
        view.poll_patches().unwrap();
        assert_eq!(titles(view.tree())[0], "first");
    }

    /// How many times each of `views` ran for the change `change` made.
    fn runs_for(
        rows: &Rows,
        views: &mut [&mut ViewInstance],
        change: impl FnOnce(&Rows),
    ) -> Vec<u64> {
        let before: Vec<u64> = views.iter().map(|v| v.stats().renders).collect();
        change(rows);
        views
            .iter_mut()
            .zip(before)
            .map(|(view, before)| {
                view.poll_patches().unwrap();
                view.stats().renders.checked_sub(before).unwrap()
            })
            .collect()
    }

    #[test]
    fn a_view_depends_on_what_it_read_of_a_collection_not_on_all_of_it() {
        let rows = Rows::new(1_000);
        let mut oldest = rows.view("oldest");
        let mut newest = rows.view("newest");
        let mut count = rows.view("count");
        assert_eq!(texts(oldest.tree()), ["item 0"]);
        assert_eq!(texts(newest.tree()), ["item 999"]);
        assert_eq!(texts(count.tree()), ["1000"]);
        let mut views = [&mut oldest, &mut newest, &mut count];
        let remove = |i: usize| {
            move |rows: &Rows| {
                rows.call(
                    "remove",
                    vec![Value::resource(&rows.bag), Value::resource(&rows.items[i])],
                );
            }
        };

        // An append changes the last item and the length, not the first.
        let add = |rows: &Rows| {
            rows.call("add", vec![Value::resource(&rows.bag), Value::text("new")]);
        };
        assert_eq!(runs_for(&rows, &mut views, add), [0, 1, 1]);
        // A move in the middle changes none of them.
        let shuffle = |rows: &Rows| {
            rows.call(
                "move_to",
                vec![
                    Value::resource(&rows.bag),
                    Value::resource(&rows.items[500]),
                    Value::Int(200),
                ],
            );
        };
        assert_eq!(runs_for(&rows, &mut views, shuffle), [0, 0, 0]);
        // Removing the first item changes the first and the length, not the last.
        assert_eq!(runs_for(&rows, &mut views, remove(0)), [1, 0, 1]);
        // Removing one in the middle changes the length alone.
        assert_eq!(runs_for(&rows, &mut views, remove(300)), [0, 0, 1]);

        assert_eq!(texts(oldest.tree()), ["item 1"]);
        assert_eq!(texts(newest.tree()), ["new"]);
        assert_eq!(texts(count.tree()), ["999"]);
    }

    #[test]
    fn a_lists_view_depends_on_the_rows_it_shows_not_on_the_rest() {
        let rows = Rows::new(1_000);
        let mut view = rows.view("lazy");
        let list = list_key(&view);
        for i in 0..20 {
            view.row(&list, i).unwrap();
        }
        let opened = view.stats().renders;
        // A move far below the shown rows re-runs nothing that is shown.
        rows.call(
            "move_to",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[900]),
                Value::Int(800),
            ],
        );
        view.poll_patches().unwrap();
        assert_eq!(
            view.stats().renders - opened,
            1,
            "the list view ran, no shown row"
        );
        assert_eq!(titles(view.row(&list, 800).unwrap().unwrap()), ["item 900"]);
    }
}
