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

action check(item: Item) {
    item.done = true
}

enum Which {
    all
    open
}

fn shown(bag: Bag, which: Which) -> List<Item> {
    match which {
        .all => bag.items,
        .open => bag.items.filter(item -> not item.done),
    }
}

view chosen(bag: Bag) {
    window which: Which = .all
    list(shown(bag, which), filter: which) { item ->
        row(item.title, checkbox: item.done)
    }
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

view done_count(bag: Bag) {
    section(bag.name) {
        text("{bag.items.count(item -> not item.done)} open")
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
        // The rows followed the collection: the one new row ran once, the bag view and no other
        // row ran.
        assert_eq!(view.stats().renders - opened.renders, 1);
    }

    #[test]
    fn a_lists_rows_render_when_shown_and_stop_when_hidden() {
        let rows = Rows::new(1_000);
        let mut view = rows.view("lazy");
        let list = list_key(&view);
        assert_eq!(view.list_len(&list), Some(1_000));
        let opened = view.stats();
        assert_eq!(opened.renders, 1, "no row ran before it was shown");
        // Nor was one built: the list costs the rows on screen, not its length.
        assert_eq!(opened.boundaries, 1, "{opened:?}");
        assert_eq!(view.tree().count(), 1, "{:?}", view.tree());
        let final_row = view.row_key(&list, 999).unwrap();
        assert!(
            final_row.ends_with(&format!("/{}", rows.items[999].id_string())),
            "{final_row}"
        );
        assert_eq!(view.row_key(&list, 1_000), None);

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
            0,
            "the rows followed; nothing ran"
        );
        assert_eq!(titles(view.row(&list, 800).unwrap().unwrap()), ["item 900"]);
    }

    #[test]
    fn a_lists_rows_follow_inserts_and_removes_without_running_the_view() {
        let rows = Rows::new(1_000);
        let mut view = rows.view("lazy");
        let list = list_key(&view);
        for i in 0..20 {
            view.row(&list, i).unwrap();
        }
        let opened = view.stats().renders;

        // An item moved to the top: one patch, and the shown rows below it move down.
        rows.call(
            "move_to",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[700]),
                Value::Int(0),
            ],
        );
        let moved = view.poll_patches().unwrap();
        assert!(
            matches!(moved.as_slice(), [Patch::Move { to: 0, .. }]),
            "{moved:?}"
        );
        assert_eq!(view.stats().renders, opened, "nothing ran");
        assert_eq!(titles(view.row(&list, 0).unwrap().unwrap()), ["item 700"]);
        let shown = view.stats().renders;
        assert_eq!(shown, opened + 1, "the moved row ran when it was shown");

        // An appended item: one patch, a row not shown yet.
        rows.call("add", vec![Value::resource(&rows.bag), Value::text("new")]);
        let added = view.poll_patches().unwrap();
        assert!(
            matches!(added.as_slice(), [Patch::Insert { index: 1_000, .. }]),
            "{added:?}"
        );
        assert_eq!(view.list_len(&list), Some(1_001));
        assert_eq!(view.stats().renders, shown);

        // A shown row that moved down still follows its item, at its new place.
        rows.retitle(5, "five");
        let retitled = view.poll_patches().unwrap();
        assert_eq!(retitled.len(), 1, "{retitled:?}");
        assert_eq!(view.stats().renders, shown + 1);
        let row = &of_kind(view.tree(), "list")[0].children[6];
        assert_eq!(titles(row), ["five"]);

        // A removed item takes its row with it; the rows below keep following.
        rows.call(
            "remove",
            vec![Value::resource(&rows.bag), Value::resource(&rows.items[2])],
        );
        let removed = view.poll_patches().unwrap();
        assert!(
            matches!(removed.as_slice(), [Patch::Remove { .. }]),
            "{removed:?}"
        );
        rows.retitle(5, "fünf");
        view.poll_patches().unwrap();
        let row = &of_kind(view.tree(), "list")[0].children[5];
        assert_eq!(titles(row), ["fünf"]);

        // Hidden after it moved, a row is forgotten where it is now.
        let hidden = view.hide_rows(&list, 0..3);
        assert!(hidden.iter().all(|p| matches!(p, Patch::Remove { .. })));
        let titles_now: Vec<String> = (0..10)
            .map(|i| titles(view.row(&list, i).unwrap().unwrap()).concat())
            .collect();
        assert_eq!(
            titles_now,
            [
                "item 700", "item 0", "item 1", "item 3", "item 4", "fünf", "item 6", "item 7",
                "item 8", "item 9"
            ]
        );
    }

    /// Every row key of `list`, as the host reads them.
    fn row_keys(view: &ViewInstance, list: &str) -> Vec<String> {
        (0..view.list_len(list).unwrap())
            .map(|i| view.row_key(list, i).unwrap())
            .collect()
    }

    /// Apply `patches` to the row keys `held` of `list`, as a host keeps them; a list replaced
    /// whole is read anew.
    fn follow(view: &ViewInstance, list: &str, held: &mut Vec<String>, patches: &[Patch]) {
        for patch in patches {
            match patch {
                Patch::Insert {
                    parent,
                    index,
                    node,
                } if parent.last().map(String::as_str) == Some(list) => {
                    held.insert(*index, node.key.clone());
                }
                Patch::Remove { parent, key }
                    if parent.last().map(String::as_str) == Some(list) =>
                {
                    let at = held.iter().position(|k| k == key).unwrap();
                    held.remove(at);
                }
                Patch::Move { parent, key, to }
                    if parent.last().map(String::as_str) == Some(list) =>
                {
                    let at = held.iter().position(|k| k == key).unwrap();
                    let row = held.remove(at);
                    held.insert(*to, row);
                }
                Patch::SetRoot(_) => *held = row_keys(view, list),
                Patch::Insert { node, .. } if node.key == list => {
                    *held = row_keys(view, list);
                }
                _ => {}
            }
        }
    }

    /// The keys of the rows built in the tree are among the rows, in their order.
    fn built_in_order(view: &ViewInstance, list: &str, held: &[String]) {
        let built: Vec<&String> = of_kind(view.tree(), "list")
            .into_iter()
            .find(|n| n.key == list)
            .unwrap()
            .children
            .iter()
            .map(|row| &row.key)
            .collect();
        let mut rest = held.iter();
        for key in &built {
            assert!(rest.any(|k| &k == key), "{key} out of order in {built:?}");
        }
    }

    #[test]
    fn a_virtual_lists_rows_follow_steps_and_a_filter_chosen() {
        let rows = Rows::new(30);
        for i in [1, 3, 5] {
            rows.call("check", vec![Value::resource(&rows.items[i])]);
        }
        let mut view = rows.view("chosen");
        let list = list_key(&view);
        let mut held = row_keys(&view, &list);
        assert_eq!(held.len(), 30);
        for i in 0..10 {
            view.row(&list, i).unwrap();
        }
        let built = view.stats().boundaries;

        // A new item: one row more for the host, none built.
        rows.call("add", vec![Value::resource(&rows.bag), Value::text("new")]);
        let added = view.poll_patches().unwrap();
        follow(&view, &list, &mut held, &added);
        assert_eq!(held, row_keys(&view, &list));
        assert_eq!(view.stats().boundaries, built, "{added:?}");

        // A shown row moved down, and one moved into the rows shown.
        rows.call(
            "move_to",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[2]),
                Value::Int(20),
            ],
        );
        rows.call(
            "move_to",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[25]),
                Value::Int(4),
            ],
        );
        let moved = view.poll_patches().unwrap();
        follow(&view, &list, &mut held, &moved);
        assert_eq!(held, row_keys(&view, &list));
        built_in_order(&view, &list, &held);

        // A shown row removed.
        rows.call(
            "remove",
            vec![Value::resource(&rows.bag), Value::resource(&rows.items[0])],
        );
        let removed = view.poll_patches().unwrap();
        follow(&view, &list, &mut held, &removed);
        assert_eq!(held, row_keys(&view, &list));
        built_in_order(&view, &list, &held);

        // Another filter: the rows that stay are kept, the others leave.
        view.fire(&list, "filter.open").unwrap();
        let chosen = view.poll_patches().unwrap();
        follow(&view, &list, &mut held, &chosen);
        assert_eq!(held, row_keys(&view, &list));
        assert_eq!(held.len(), 27, "{chosen:?}");
        built_in_order(&view, &list, &held);
        let titles_now: Vec<String> = (0..4)
            .map(|i| titles(view.row(&list, i).unwrap().unwrap()).concat())
            .collect();
        assert_eq!(titles_now, ["item 4", "item 25", "item 6", "item 7"]);
    }

    #[test]
    fn a_view_that_reads_every_item_follows_every_change() {
        let rows = Rows::new(300);
        let mut view = rows.view("done_count");
        assert_eq!(texts(view.tree()), ["300 open"]);
        rows.call("add", vec![Value::resource(&rows.bag), Value::text("new")]);
        view.poll_patches().unwrap();
        assert_eq!(texts(view.tree()), ["301 open"]);
        rows.call(
            "remove",
            vec![Value::resource(&rows.bag), Value::resource(&rows.items[7])],
        );
        view.poll_patches().unwrap();
        assert_eq!(texts(view.tree()), ["300 open"]);
    }
}
