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

    search {
        filter "Open" = not done
        filter done
    }
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

action move_before(bag: Bag, item: Item, other: Item) {
    bag.items.move(item, before: other)
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

action uncheck(item: Item) {
    item.done = false
}

action open_count(bag: Bag) -> Int {
    return bag.items.count(item -> not item.done)
}

action open_titles(bag: Bag) -> List<Text> {
    return bag.items.filter(item -> not item.done).map(item -> item.title)
}

view chosen(bag: Bag) {
    state place: Text = ""
    // The item this window's list shows opened where it stands.
    state opened: Option<Item> = .none
    let shown = bag.items.matching(place)
    section("Items") {
        std.places(place, of: filters(Item).filter(f -> f.toggle), everything: "All", icon: .none)
        std.list(shown, opened) { (item, at) ->
            ui.ListItem(item: item, presence: at) {
                item_line(item)
            }
        }
    }
}

view item_line(item: Item) {
    row(item.title, checkbox: item.done)
}

/// A list whose rows open into their item's document where they stand: the document is built
/// for the row standing open, and for no other row on screen.
view documents(bag: Bag) {
    state opened: Option<Item> = .none
    std.list(bag.items, opened) { (item, at) ->
        ui.ListItem(item: item, presence: at) {
            item_line(item)
            detail: {
                item_document(item)
            }
        }
    }
}

view item_document(item: Item) {
    section("About {item.title}") {
        text("what {item.title} is for")
        checkbox("Done", bind: item.done)
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

view open_card(bag: Bag) {
    section(bag.name) {
        text("{bag.items.count(item -> not item.done and item.title != "skip")} open")
        list(bag.items.filter(item -> not item.done)) { item ->
            row(item.title)
        }
    }
}

view handed(bag: Bag) {
    rows_of(bag.items)
}

view rows_of(items: Collection<Item>) {
    section("Items") {
        for item in items {
            item_row(item)
        }
    }
}

view count(bag: Bag) {
    section(bag.name) {
        text("{bag.items.len()}")
    }
}

view tinted(bag: Bag) {
    section(bag.name) {
        list(bag.items.filter(item -> not item.done)) { item ->
            tinted_row(item, bag.name)
        }
    }
}

view tinted_row(item: Item, tint: Text) {
    row {
        text(item.title)
        text(tint)
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
        of_kind(tree, "ui.Text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    /// The titles of the list rows under `tree`.
    fn titles(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.ListRow")
            .iter()
            .map(|t| text_prop(t, "title"))
            .collect()
    }

    fn list_key(view: &ViewInstance) -> String {
        lists(view.tree())[0].key.clone()
    }

    /// The lists under `tree` whose rows render as they show: the content's and the toolkit's.
    fn lists(tree: &UiNode) -> Vec<&UiNode> {
        let mut out = of_kind(tree, "list");
        out.extend(of_kind(tree, "ui.VirtualList"));
        out
    }

    /// Choose the place `name` of the window's filter, in its sidebar.
    fn choose(view: &ViewInstance, name: &str) {
        let place = of_kind(view.tree(), "ui.NavItem")
            .into_iter()
            .find(|item| text_prop(item, "key") == name)
            .unwrap_or_else(|| panic!("no place {name}"))
            .key
            .clone();
        view.fire(&place, "select").unwrap();
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
    fn a_collection_handed_to_a_view_follows_one_insert_with_one_row() {
        let rows = Rows::new(2000);
        let mut view = rows.view("handed");
        let opened = view.stats();
        assert_eq!(
            opened.boundaries, 2002,
            "the view, the one it hands to, a row per item"
        );
        let built = rows.rt.items_built();
        rows.call(
            "add",
            vec![Value::resource(&rows.bag), Value::text("one more")],
        );
        let patches = view.poll_patches().unwrap();
        assert_eq!(
            view.stats().renders - opened.renders,
            1,
            "only the new item's row ran: neither view holding the collection did"
        );
        assert!(
            rows.rt.items_built() - built <= 2,
            "the collection was not read whole: {} items built",
            rows.rt.items_built() - built
        );
        assert!(
            matches!(patches.as_slice(), [Patch::Insert { index: 2000, .. }]),
            "{patches:?}"
        );
        assert_eq!(
            texts(view.tree()).last().map(String::as_str),
            Some("one more")
        );
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
            text_prop(of_kind(view.tree(), "ui.Section")[0], "title"),
            "Groceries"
        );
    }

    #[test]
    fn moves_inserts_and_removes_keep_the_other_rows() {
        let rows = Rows::new(6);
        let mut view = rows.view("eager");
        let opened = view.stats();
        rows.call(
            "move_before",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[4]),
                Value::resource(&rows.items[1]),
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
    fn a_rows_argument_from_outside_its_item_reaches_the_row() {
        // The rows of the list are the same items, and the row view's other argument changed:
        // the rows shown stand for the new one, not for the one they were built with.
        let rows = Rows::new(10);
        let mut view = rows.view("tinted");
        let list = list_key(&view);
        for i in 0..3 {
            assert!(view.row(&list, i).unwrap().is_some());
        }
        let shown = |view: &mut ViewInstance| texts(view.row(&list, 1).unwrap().unwrap());
        assert_eq!(shown(&mut view), ["item 1", ""]);

        rows.call(
            "rename",
            vec![Value::resource(&rows.bag), Value::text("groceries")],
        );
        let patches = view.poll_patches().unwrap();
        assert!(!patches.is_empty(), "the shown rows ran again");
        assert_eq!(shown(&mut view), ["item 1", "groceries"]);
    }

    /// The document of a row is built for the row standing open into its item, and for no other
    /// row on screen: a list of ten thousand tasks costs the one page a person reads, and the
    /// rows that stand closed carry none of theirs.
    #[test]
    fn a_rows_document_is_built_for_the_row_standing_open_alone() {
        let rows = Rows::new(100);
        let mut view = rows.view("documents");
        let list = list_key(&view);
        let opened = view.stats();
        for i in 0..10 {
            view.row(&list, i).unwrap();
        }
        // Every row shows its item's line, and none shows a document: the row's `detail` slot
        // holds what the item is about only while the row stands open into it.
        for i in 0..10 {
            let row = view.row(&list, i).unwrap().unwrap();
            assert_eq!(titles(row), [format!("item {i}")]);
            assert!(
                texts(row).iter().all(|text| !text.contains("what item")),
                "row {i} built the document of an item nobody opened"
            );
        }
        // Two boundaries per row: the row and the line its item shows. A document would be a
        // third.
        assert_eq!(
            view.stats().renders - opened.renders,
            20,
            "showing ten rows ran ten rows and their lines, and no document"
        );

        // The person opens the fourth row where it stands: its document arrives, built once.
        let fourth = view.row_key(&list, 3).expect("the fourth row");
        let before = view.stats().renders;
        view.open_row(&list, Some(&fourth));
        let patches = view.poll_patches().unwrap();
        let opened_row = view.row(&list, 3).unwrap().unwrap();
        assert!(
            texts(opened_row)
                .iter()
                .any(|text| text == "what item 3 is for"),
            "the row standing open shows its item's document: {opened_row:?}"
        );
        assert!(
            patches
                .iter()
                .any(|patch| matches!(patch, Patch::Insert { .. })),
            "the document is sent where it stands: {patches:?}"
        );
        // The row, the line it shows, and the document it now stands open into.
        assert_eq!(
            view.stats().renders - before,
            3,
            "opening a row runs it, its line and its document, and no other row"
        );
        for i in 0..10 {
            if i == 3 {
                continue;
            }
            let row = view.row(&list, i).unwrap().unwrap();
            assert!(
                texts(row).iter().all(|text| !text.contains("what item")),
                "row {i} built a document of its own beside the one the person reads"
            );
        }

        // A change to the open row's item reaches its document as it ever did.
        rows.retitle(3, "three");
        view.poll_patches().unwrap();
        let opened_row = view.row(&list, 3).unwrap().unwrap();
        assert!(
            texts(opened_row)
                .iter()
                .any(|text| text == "what three is for"),
            "{opened_row:?}"
        );

        // Closed again, the row drops its document, and no document of it runs from then on.
        let before = view.stats().renders;
        view.open_row(&list, None);
        let patches = view.poll_patches().unwrap();
        let closed = view.row(&list, 3).unwrap().unwrap();
        assert!(
            texts(closed).iter().all(|text| !text.contains("what")),
            "the row that closed dropped its document: {closed:?}"
        );
        assert!(
            patches
                .iter()
                .any(|patch| matches!(patch, Patch::Remove { .. })),
            "the document is taken where it stood: {patches:?}"
        );
        // The row and the line it shows again; the document is gone, not built.
        assert_eq!(
            view.stats().renders - before,
            2,
            "closing a row runs it and its line again, and builds no document"
        );
        // The row still shows its item's line, and runs for that alone: the document of the
        // item it no longer stands open into is not built again.
        let before = view.stats().renders;
        rows.retitle(3, "drei");
        assert!(!view.poll_patches().unwrap().is_empty());
        assert_eq!(titles(view.row(&list, 3).unwrap().unwrap()), ["drei"]);
        assert!(
            texts(view.row(&list, 3).unwrap().unwrap())
                .iter()
                .all(|text| !text.contains("what")),
            "the closed row builds no document when its item changes"
        );
        assert!(
            view.stats().renders - before <= 3,
            "the closed row's item changing runs the view that holds the list, the row and its \
             line, and no document: {} ran",
            view.stats().renders - before
        );
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
                "move_before",
                vec![
                    Value::resource(&rows.bag),
                    Value::resource(&rows.items[500]),
                    Value::resource(&rows.items[200]),
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
            "move_before",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[900]),
                Value::resource(&rows.items[800]),
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
            "move_before",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[700]),
                Value::resource(&rows.items[0]),
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
        let row = &lists(view.tree())[0].children[6];
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
        let row = &lists(view.tree())[0].children[5];
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
                Patch::Remove { parent, key, .. }
                    if parent.last().map(String::as_str) == Some(list) =>
                {
                    let at = held.iter().position(|k| k == key).unwrap();
                    held.remove(at);
                }
                Patch::Move {
                    parent, key, to, ..
                } if parent.last().map(String::as_str) == Some(list) => {
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
        let built: Vec<&String> = lists(view.tree())
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
            "move_before",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[2]),
                Value::resource(&rows.items[21]),
            ],
        );
        rows.call(
            "move_before",
            vec![
                Value::resource(&rows.bag),
                Value::resource(&rows.items[25]),
                Value::resource(&rows.items[5]),
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

        // Another filter: rows of another query, which the host reads anew; the list builds
        // the rows it shows.
        choose(&view, "is:open");
        let chosen = view.poll_patches().unwrap();
        assert!(
            chosen
                .iter()
                .any(|p| matches!(p, Patch::Insert { node, .. } if node.key == list)),
            "the host reads the rows anew: {chosen:?}"
        );
        assert!(chosen.len() < 10, "no step per row: {chosen:?}");
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
    fn a_row_the_viewer_checks_out_of_a_query_keeps_its_place() {
        let rows = Rows::new(6);
        let mut view = rows.view("chosen");
        choose(&view, "is:open");
        view.poll_patches().unwrap();
        let list = list_key(&view);
        let before = row_keys(&view, &list);
        assert_eq!(before.len(), 6, "all six are open: {before:?}");
        // The person checks the third off: the query leaves it out, and its row stands.
        rows.call("check", vec![Value::resource(&rows.items[2])]);
        let patches = view.poll_patches().unwrap();
        assert!(
            !patches.iter().any(|p| matches!(
                p,
                Patch::Insert { .. } | Patch::Remove { .. } | Patch::Move { .. }
            )),
            "no row moves: {patches:?}"
        );
        assert_eq!(
            row_keys(&view, &list),
            before,
            "the row stands where it was"
        );
        assert_eq!(
            titles(view.row(&list, 2).unwrap().unwrap()).concat(),
            "item 2",
            "the row still presents the item"
        );

        // Leaving the place settles the list: the checked item is not one of the open ones.
        choose(&view, "all");
        view.poll_patches().unwrap();
        choose(&view, "is:open");
        view.poll_patches().unwrap();
        let settled = row_keys(&view, &list);
        assert_eq!(
            settled.len(),
            5,
            "the checked item is not open: {settled:?}"
        );
        assert!(!settled.contains(&before[2]));
    }

    /// Rows a person checks one after another stand where they are: each kept row keeps its
    /// own place, and the list goes on rendering.
    #[test]
    fn rows_checked_one_after_another_keep_their_places() {
        let rows = Rows::new(6);
        let mut view = rows.view("chosen");
        choose(&view, "is:open");
        view.poll_patches().unwrap();
        let list = list_key(&view);
        let before = row_keys(&view, &list);
        for i in 0..3 {
            rows.call("check", vec![Value::resource(&rows.items[i])]);
            let patches = view.poll_patches().unwrap();
            assert!(
                !patches.iter().any(|p| matches!(
                    p,
                    Patch::Insert { .. } | Patch::Remove { .. } | Patch::Move { .. }
                )),
                "no row moves: {patches:?}"
            );
            assert_eq!(
                row_keys(&view, &list),
                before,
                "the rows the person checked stay: {patches:?}"
            );
            for at in 0..=i {
                assert_eq!(
                    titles(view.row(&list, at).unwrap().unwrap()).concat(),
                    format!("item {at}"),
                    "the row checked at {at} stays where it stood"
                );
            }
        }
        let keys = row_keys(&view, &list);
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "each row is one row: {keys:?}");
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

    /// A count and a list of the items a lambda over their fields holds of are answered by the
    /// store: opening builds no reference per item, and a change to an item's field moves it
    /// in or out.
    #[test]
    fn a_filter_over_item_fields_is_answered_by_the_store() {
        let rows = Rows::new(300);
        for i in [1, 3] {
            rows.call("check", vec![Value::resource(&rows.items[i])]);
        }
        rows.retitle(0, "skip");
        let built = rows.rt.items_built();
        let mut view = rows.view("open_card");
        assert_eq!(texts(view.tree()), ["297 open"]);
        assert_eq!(
            rows.rt.items_built() - built,
            0,
            "the count and the list's length build no reference"
        );
        let list = list_key(&view);
        let preview = |view: &mut ViewInstance| -> Vec<String> {
            (0..3)
                .map(|i| titles(view.row(&list, i).unwrap().unwrap()).concat())
                .collect()
        };
        assert_eq!(preview(&mut view), ["skip", "item 2", "item 4"]);

        // Checked: out of the count, and its row standing where it was until the view is left.
        rows.call("check", vec![Value::resource(&rows.items[2])]);
        view.poll_patches().unwrap();
        assert_eq!(texts(view.tree()), ["296 open"]);
        assert_eq!(preview(&mut view), ["skip", "item 2", "item 4"]);

        // A view opened anew lists only the open items.
        let mut view = rows.view("open_card");
        assert_eq!(texts(view.tree()), ["296 open"]);
        assert_eq!(preview(&mut view), ["skip", "item 4", "item 5"]);

        // Unchecked: back in, where it was.
        rows.call("uncheck", vec![Value::resource(&rows.items[1])]);
        view.poll_patches().unwrap();
        assert_eq!(texts(view.tree()), ["297 open"]);
        assert_eq!(preview(&mut view), ["skip", "item 1", "item 4"]);

        // Retitled out of what the count holds of, and a new open item.
        rows.retitle(4, "skip");
        rows.call("add", vec![Value::resource(&rows.bag), Value::text("new")]);
        view.poll_patches().unwrap();
        assert_eq!(texts(view.tree()), ["297 open"]);

        // An action runs the same lambdas as loops, and finds the same.
        let InvokeOutcome::Done(Value::Int(open)) =
            rows.call("open_count", vec![Value::resource(&rows.bag)])
        else {
            panic!("a count");
        };
        assert_eq!(open, 299);
        let InvokeOutcome::Done(Value::List(open)) =
            rows.call("open_titles", vec![Value::resource(&rows.bag)])
        else {
            panic!("a list");
        };
        assert_eq!(open.len(), 299);
        assert_eq!(open[..2], [Value::text("skip"), Value::text("item 1")]);
    }

    /// The rows of a list of a filtered collection follow its items in and out of the filter.
    #[test]
    fn a_virtual_list_of_a_filtered_collection_follows_its_items() {
        let rows = Rows::new(30);
        let mut view = rows.view("chosen");
        let list = list_key(&view);
        choose(&view, "is:open");
        let mut held = row_keys(&view, &list);
        let chosen = view.poll_patches().unwrap();
        follow(&view, &list, &mut held, &chosen);
        assert_eq!(held.len(), 30);
        for i in 0..10 {
            view.row(&list, i).unwrap();
        }
        rows.call("check", vec![Value::resource(&rows.items[2])]);
        rows.call("check", vec![Value::resource(&rows.items[20])]);
        let checked = view.poll_patches().unwrap();
        follow(&view, &list, &mut held, &checked);
        assert_eq!(held, row_keys(&view, &list));
        assert_eq!(
            held.len(),
            30,
            "the checked rows stand where they are: {checked:?}"
        );
        built_in_order(&view, &list, &held);

        // Leaving the view settles the rows: the checked items are not open.
        choose(&view, "all");
        view.poll_patches().unwrap();
        choose(&view, "is:open");
        let settled = view.poll_patches().unwrap();
        follow(&view, &list, &mut held, &settled);
        assert_eq!(held, row_keys(&view, &list));
        assert_eq!(held.len(), 28, "{settled:?}");
        built_in_order(&view, &list, &held);
        rows.call("uncheck", vec![Value::resource(&rows.items[2])]);
        let unchecked = view.poll_patches().unwrap();
        follow(&view, &list, &mut held, &unchecked);
        assert_eq!(held, row_keys(&view, &list));
        assert_eq!(held.len(), 29);
        built_in_order(&view, &list, &held);
        let titles_now: Vec<String> = (0..3)
            .map(|i| titles(view.row(&list, i).unwrap().unwrap()).concat())
            .collect();
        assert_eq!(titles_now, ["item 0", "item 1", "item 2"]);
    }

    // ----- a view call whose branch leaves the tree ------------------------------------------

    /// A view whose two branches both stand, each holding a call of the same view, so two
    /// boundaries can want one key. A record change leaves both to run; neither may write
    /// where the whole view's content belongs.
    const SWAPPING: &str = r#"app so.tilo.swap "Swap"

type Board {
    way: Text
    said: Text
}

root Board

default view board for Board

action switch(b: Board) {
    b.way = "one"
}

action switch_back(b: Board) {
    b.way = ""
}

action say(b: Board, s: Text) {
    b.said = s
}

view chosen(b: Board) {
    section("Chosen") {
        text("chosen says {b.said}")
    }
}

view board(b: Board) {
    stack {
        text("a banner")
        if b.way == "one" {
            chosen(b)
        } else {
            chosen(b)
        }
        text("a footer")
    }
}
"#;

    fn swapping() -> (Runtime, App, ResourceRef, ViewInstance) {
        let app = App::compile(SWAPPING, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let board = rt.create("Board").unwrap();
        let view = rt
            .open_view(&me(), &app.view("board"), vec![Value::resource(&board)])
            .unwrap();
        (rt, app, board, view)
    }

    fn call(rt: &Runtime, app: &App, name: &str, args: Vec<Value>) {
        let id = app.action(name);
        rt.invoke_action(&me(), (id.as_str(), args)).unwrap();
    }

    /// The whole view, as its text, in order: every line the board draws.
    fn lines(view: &ViewInstance) -> Vec<String> {
        texts(view.tree())
    }

    /// A boundary that the tree no longer holds must not write where the whole view's content
    /// belongs. A view call's `fragment` leaves the tree when the branch holding it swaps, but
    /// the boundary made for it is still a boundary of the view, still marked to run by the
    /// change it read, and its cached path is then the empty trail of a call never placed — the
    /// trail that names the root. Writing there put that one call's content over the whole
    /// view, and the diff that followed dropped every other child of the screen.
    ///
    /// The shape that reaches it has the same call in both branches: the boundary the departing
    /// branch made is left behind when the other branch's call takes the key, and the change
    /// that follows asks it to run.
    ///
    /// Through the public interface a swap clears the departing boundary's dirtiness, so this
    /// test passes with and without the guard: it states the behaviour, not the mechanism. The
    /// mechanism — a planted orphan whose cached trail is the empty one — is
    /// `view_instance::orphan_tests::a_boundary_the_tree_does_not_hold_writes_nowhere`, which
    /// fails without the guard.
    #[test]
    fn a_boundary_the_tree_no_longer_holds_does_not_replace_the_whole_view() {
        let (rt, app, board, mut view) = swapping();
        assert_eq!(
            lines(&view),
            ["a banner", "chosen says ", "a footer"],
            "the first branch draws"
        );

        // The branch swaps: the chosen call leaves the tree where it stood, and the other
        // branch's call takes its place.
        call(&rt, &app, "switch", vec![Value::resource(&board)]);
        view.poll_patches().unwrap();
        assert_eq!(
            lines(&view),
            ["a banner", "chosen says ", "a footer"],
            "the other branch draws"
        );

        // The record changes. Every boundary that read it runs, including the one the departing
        // branch left: none of them may stand in for the whole view.
        call(
            &rt,
            &app,
            "say",
            vec![Value::resource(&board), Value::text("hello")],
        );
        view.poll_patches().unwrap();
        assert_eq!(
            lines(&view),
            ["a banner", "chosen says hello", "a footer"],
            "the view is whole: the branch that stands drew the change"
        );
        let stack = of_kind(view.tree(), "stack")
            .into_iter()
            .max_by_key(|node| node.children.len())
            .expect("the view's stack");
        assert_eq!(
            stack.children.len(),
            3,
            "the stack keeps every child: {}",
            dump(stack)
        );
    }
}
