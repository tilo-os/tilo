//! The layout examples: trees with unit-tagged props, tokens, flags and `space` reads.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Patch, Runtime, SimpleAuthority, Space, UiNode, Value};

    const SOURCE: &str = "app so.tilo.library \"Library\"\n\
type Album {\n    title: Text\n    artist: Text\n}\n\
type Library {\n    albums: Collection<Album>\n}\n\
action add(l: Library, title: Text, artist: Text) {\n    l.albums.insert(Album(title: title, artist: artist))\n}\n\
view album_card(a: Album) {\n    layers {\n        image(a.title, aspect: 16/9)\n        text(a.artist, at: bottom_trailing)\n    }\n    text(a.title, lines: 1, truncate)\n    if not space.compact {\n        text(a.artist)\n    }\n}\n\
view library(l: Library) {\n    page {\n        heading(\"Albums\")\n        grid(min: 11rem) {\n            for a in l.albums {\n                album_card(a, fill(2))\n            }\n        }\n    }\n}\n\
view detail(a: Album) {\n    page(full) {\n        split(side: leading, collapse: drawer) {\n            primary(min: 18rem) {\n                text(a.title)\n            }\n            secondary(min: 22rem) {\n                scroll {\n                    text(a.artist, min: 4dp, max: 1.5rem)\n                }\n            }\n        }\n    }\n}\n\
view bar(a: Album) {\n    if space.compact {\n        row(distribute: between) { text(a.title); spacer }\n    } else {\n        row(gap: section) {\n            text(a.title)\n            spacer\n            text(a.artist, fill)\n        }\n    }\n}";

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

    #[test]
    fn the_library_view_carries_units_tokens_and_weights_through_the_tree() {
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
        let image = first(tree, "image");
        assert!(matches!(image.props["aspect"], Value::Float(f) if (f - 16.0 / 9.0).abs() < 1e-9));
        let layered_text = first(first(tree, "layers"), "text");
        assert_eq!(text_prop(layered_text, "at"), "bottom_trailing");
        let titles: Vec<&UiNode> = of_kind(tree, "text")
            .into_iter()
            .filter(|t| t.props.contains_key("truncate"))
            .collect();
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[0].props["lines"], Value::Int(1));
        assert_eq!(titles[0].props["truncate"], Value::Int(1));
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
        let clamped = first(first(tree, "scroll"), "text");
        assert_eq!(clamped.props["min.dp"], Value::Float(4.0));
        assert_eq!(clamped.props["max.rem"], Value::Float(1.5));
    }

    #[test]
    fn a_view_that_reads_space_follows_the_class_it_is_offered() {
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
        assert_eq!(of_kind(view.tree(), "text").len(), 2);

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
        assert_eq!(of_kind(view.tree(), "text").len(), 1);

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

    #[test]
    fn the_album_card_hides_the_artist_when_compact() {
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
        let wide = of_kind(view.tree(), "text").len();
        view.set_space("", Space::from_rem(10.0, 20.0));
        view.poll_patches().unwrap();
        assert_eq!(of_kind(view.tree(), "text").len(), wide.saturating_sub(1));
    }
}
