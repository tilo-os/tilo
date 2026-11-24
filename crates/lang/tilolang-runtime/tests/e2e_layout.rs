//! The layout examples: trees with unit-tagged props, tokens, flags and `space` reads.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, Patch, Runtime, SimpleAuthority, Space, UiNode, Value};

    const SOURCE: &str = "app so.tilo.library \"Library\"\n\
type Album {\n    title: Text\n    artist: Text\n}\n\
type Library {\n    albums: Collection<Album>\n}\n\
action add(l: Library, title: Text, artist: Text) {\n    l.albums.insert(Album(title: title, artist: artist))\n}\n\
view album_card(a: Album) {\n    layers {\n        image(a.title, alt: \"The cover\", aspect: 16/9)\n        text(a.artist, at: .bottom_trailing)\n    }\n    text(a.title, lines: 1, truncate)\n    if not space.compact {\n        text(a.artist)\n    }\n}\n\
view library(l: Library) {\n    page {\n        text(\"Albums\", role: .title)\n        grid(min: 11rem) {\n            for a in l.albums {\n                album_card(a, fill(2))\n            }\n        }\n    }\n}\n\
view detail(a: Album) {\n    page(full) {\n        split(side: .leading, collapse: .drawer) {\n            primary(min: 18rem) {\n                text(a.title)\n            }\n            secondary(min: 22rem) {\n                scroll {\n                    text(a.artist, min: 4dp, max: 1.5rem)\n                }\n            }\n        }\n    }\n}\n\
view bar(a: Album) {\n    if space.compact {\n        row(distribute: .between) { text(a.title); spacer }\n    } else {\n        row(gap: .section) {\n            text(a.title)\n            spacer\n            text(a.artist, fill)\n        }\n    }\n}\n\
view shelf(a: Album) {\n    stack {\n        shelf_line(a)\n        text(a.title)\n    }\n}\n\
view shelf_line(a: Album) {\n    if space.height < 30.0 {\n        text(\"short cover\")\n    } else {\n        text(\"tall cover\")\n    }\n}\n\
view paged(a: Album) {\n    std.page(a, canvas: true, fill: true) {\n        shelf_line(a)\n    }\n}";

    fn setup() -> (Runtime, App, tilolang_runtime::ResourceRef) {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let lib = rt.create("Library").unwrap();
        let add = app.action("add");
        for (t, a) in [("Blue", "Joni"), ("Kind of Blue", "Miles")] {
            rt.invoke_action(
                &me(),
                (
                    add.as_str(),
                    vec![Value::resource(&lib), Value::text(t), Value::text(a)],
                ),
            )
            .unwrap();
        }
        (rt, app, lib)
    }

    fn first<'a>(n: &'a UiNode, kind: &str) -> &'a UiNode {
        of_kind(n, kind)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no {kind} in\n{}", dump(n)))
    }

    tilo_test_env::scenario! {
        fn the_library_lays_out_by_units_and_the_space_it_is_offered;

        phase the_library_view_carries_units_tokens_and_weights_through_the_tree;
        {
            let (rt, app, lib) = setup();
            let view = rt
                .open_view(&me(), &app.view("library"), vec![Value::resource(&lib)])
                .unwrap();
            let tree = view.tree();
            let grid = first(tree, "grid");
            assert_eq!(grid.props["min.rem"], Value::Float(11.0), "{}", dump(tree));
            let cards: Vec<&UiNode> = grid.children.iter().collect();
            assert_eq!(cards.len(), 2);
            for card in &cards {
                assert_eq!(card.kind, "fragment");
                assert_eq!(card.props["fill"], Value::Float(2.0), "{}", dump(tree));
            }
            let image = first(tree, "ui.Image");
            assert_eq!(image.props["alt"], Value::Text("The cover".into()));
            assert!(matches!(image.props["aspect"], Value::Float(f) if (f - 16.0 / 9.0).abs() < 1e-9));
            let layered_text = first(first(tree, "layers"), "ui.Text");
            assert_eq!(text_prop(layered_text, "at"), "bottom_trailing");
            let titles: Vec<&UiNode> = of_kind(tree, "ui.Text")
                .into_iter()
                .filter(|t| t.props.contains_key("truncate"))
                .collect();
            assert_eq!(titles.len(), 2);
            assert_eq!(titles[0].props["lines"], Value::Int(1));
            assert_eq!(titles[0].props["truncate"], Value::Int(1));
        }

        phase a_view_that_reads_space_follows_the_class_it_is_offered;
        {
            let (rt, app, lib) = setup();
            let Value::List(albums) = rt.read(&lib, "albums").unwrap() else {
                panic!("albums");
            };
            let Value::Ref(album) = &albums[0] else {
                panic!("ref")
            };
            let mut view = rt
                .open_view(
                    &me(),
                    &app.view("bar"),
                    vec![Value::resource(&album.target)],
                )
                .unwrap();
            let roomy = dump(view.tree());
            assert!(
                text_prop(first(view.tree(), "row"), "gap") == "section",
                "{roomy}"
            );
            assert_eq!(of_kind(view.tree(), "ui.Text").len(), 2);

            assert!(view.set_space("", Space::from_rem(20.0, 40.0)));
            let patches = view.poll_patches().unwrap();
            assert!(!patches.is_empty());
            let compact = first(view.tree(), "row");
            assert_eq!(
                text_prop(compact, "distribute"),
                "between",
                "{}",
                dump(view.tree())
            );
            assert_eq!(of_kind(view.tree(), "ui.Text").len(), 1);

            assert!(
                !view.set_space("", Space::from_rem(25.0, 40.0)),
                "same class: nothing re-runs"
            );
            assert!(view.set_space("", Space::from_rem(60.0, 40.0)));
            assert!(matches!(
                view.poll_patches().unwrap().first(),
                Some(Patch::Remove { .. } | Patch::Insert { .. } | Patch::SetProp { .. })
            ));
            assert_eq!(dump(view.tree()), roomy);
        }

        phase the_album_card_hides_the_artist_when_compact;
        {
            let (rt, app, lib) = setup();
            let Value::List(albums) = rt.read(&lib, "albums").unwrap() else {
                panic!("albums");
            };
            let Value::Ref(album) = &albums[0] else {
                panic!("ref")
            };
            let mut view = rt
                .open_view(
                    &me(),
                    &app.view("album_card"),
                    vec![Value::resource(&album.target)],
                )
                .unwrap();
            let wide = of_kind(view.tree(), "ui.Text").len();
            view.set_space("", Space::from_rem(10.0, 20.0));
            view.poll_patches().unwrap();
            assert_eq!(
                of_kind(view.tree(), "ui.Text").len(),
                wide.saturating_sub(1)
            );
        }

        phase a_view_call_reads_the_space_layout_gives_it_and_follows_a_resize;
        {
            let (rt, app, lib) = setup();
            let Value::List(albums) = rt.read(&lib, "albums").unwrap() else {
                panic!("albums");
            };
            let Value::Ref(album) = &albums[0] else {
                panic!("ref")
            };
            let mut view = rt
                .open_view(&me(), &app.view("shelf"), vec![Value::resource(&album.target)])
                .unwrap();
            // The one boundary that reads its own size: the shelf line, not the shelf.
            let readers = view.space_readers();
            assert_eq!(readers.len(), 1, "{readers:?}");
            let root_runs = view.stats().root_runs;
            let said = |view: &tilolang_runtime::ViewInstance| text_prop(first(view.tree(), "ui.Text"), "value");
            assert_eq!(said(&view), "short cover", "{}", dump(view.tree()));

            // The space it was given is its own: the shelf's is untouched.
            assert!(view.set_space(&readers[0], Space::from_rem(40.0, 60.0)));
            let patches = view.poll_patches().unwrap();
            assert!(!patches.is_empty());
            assert_eq!(said(&view), "tall cover", "{}", dump(view.tree()));
            assert_eq!(view.stats().root_runs, root_runs, "the shelf did not run");

            // The same size again is no change; below the height it reads, it says so again.
            assert!(!view.set_space(&readers[0], Space::from_rem(40.0, 60.0)));
            assert!(view.set_space(&readers[0], Space::from_rem(40.0, 20.0)));
            view.poll_patches().unwrap();
            assert_eq!(said(&view), "short cover", "{}", dump(view.tree()));
            // The root's space is the root's: the shelf reads none, so nothing runs.
            assert!(!view.set_space("", Space::from_rem(10.0, 20.0)));
            assert_eq!(said(&view), "short cover");
        }

        phase a_view_call_in_a_page_is_a_boundary_of_its_own;
        {
            let (rt, app, lib) = setup();
            let Value::List(albums) = rt.read(&lib, "albums").unwrap() else {
                panic!("albums");
            };
            let Value::Ref(album) = &albums[0] else {
                panic!("ref")
            };
            let view = rt
                .open_view(&me(), &app.view("paged"), vec![Value::resource(&album.target)])
                .unwrap();
            let readers = view.space_readers();
            assert_eq!(readers.len(), 1, "the line reads its space in a page: {readers:?}");
        }
    }

    #[test]
    fn detail_uses_flags_slots_and_every_unit() {
        let (rt, app, lib) = setup();
        let Value::List(albums) = rt.read(&lib, "albums").unwrap() else {
            panic!("albums");
        };
        let Value::Ref(album) = &albums[0] else {
            panic!("ref")
        };
        let view = rt
            .open_view(
                &me(),
                &app.view("detail"),
                vec![Value::resource(&album.target)],
            )
            .unwrap();
        let tree = view.tree();
        assert_eq!(
            first(tree, "page").props["full"],
            Value::Int(1),
            "{}",
            dump(tree)
        );
        let split = first(tree, "split");
        assert_eq!(text_prop(split, "side"), "leading");
        assert_eq!(text_prop(split, "collapse"), "drawer");
        assert_eq!(first(tree, "primary").props["min.rem"], Value::Float(18.0));
        assert_eq!(
            first(tree, "secondary").props["min.rem"],
            Value::Float(22.0)
        );
        let clamped = first(first(tree, "scroll"), "ui.Text");
        assert_eq!(clamped.props["min.dp"], Value::Float(4.0));
        assert_eq!(clamped.props["max.rem"], Value::Float(1.5));
    }

    /// The host puts a record's actions on the record, grouped as their declarations say: a
    /// row's menu carries the run of each action on it (`group:`). This is the boundary the
    /// traditional desktop's action bar stands on.
    #[test]
    fn a_resource_read_by_id_offers_the_actions_its_declaration_grouped() {
        const ACTIONS: &str = "app so.tilo.library \"Library\"\n\
type Album {\n    title: Text\n    pinned: Bool = false\n}\n\
type Shelf {\n    albums: Collection<Album>\n}\n\
root Shelf\n\ndefault view shelf for Shelf\n\
action add(s: Shelf) -> Album (icon: .plus) {\n    s.albums.insert(Album(title: \"Blue\"))\n}\n\
action pin(a: Album, pinned: Bool = not a.pinned) (icon: .pin, on: a.pinned, group: \"Pin\") {\n    a.pinned = pinned\n}\n\
action rename(a: Album, title: Text) (icon: .pen_tool, group: \"Edit\") {\n    a.title = title\n}\n\
view shelf(s: Shelf) {\n    ui.NavShell(content_kind: .list, title: meta(s).title) {\n        content: {\n            stack {\n                for a in s.albums {\n                    ui.ListItem(item: a) {\n                        ui.ListRow(title: a.title)\n                    }\n                }\n            }\n        }\n    }\n}\n\
view album(a: Album) {\n    text(a.title)\n}";
        let app = App::compile(ACTIONS, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let shelf = rt.create_root("Shelf").unwrap();
        rt.invoke_action(
            &me(),
            (app.action("add").as_str(), vec![Value::resource(&shelf)]),
        )
        .unwrap();
        let view = rt
            .open_view(&me(), &app.view("shelf"), vec![Value::resource(&shelf)])
            .unwrap();
        let row = *of_kind(view.tree(), "ui.ListItem")
            .first()
            .unwrap_or_else(|| panic!("a row per album: {}", dump(view.tree())));
        let groups: Vec<String> = of_kind(row, "ui.MenuItem")
            .into_iter()
            .filter_map(|item| match item.props.get("group") {
                Some(Value::Text(group)) => Some(group.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(groups, ["Pin", "Edit"], "{}", dump(view.tree()));
    }
}
