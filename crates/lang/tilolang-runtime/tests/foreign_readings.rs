//! A view over another app's reading whose fields are not bare values: the window menu's rows,
//! a list of objects the shell offers a window. Each row reads as the record its app declares —
//! its label, whether it can be chosen and why not — and a reading that does not match the
//! declared types is an error that says which field of which row.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::json;
    use tilolang_runtime::{
        AmStore, Meta, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
    };

    /// The window menu as the shell publishes it: the window it is for, where it stands, and the
    /// rows that window offers now (`desktop:window-menu`).
    const READING: &str = r#"{
        "tab": "20",
        "anchor": { "x": 40, "y": 50 },
        "rows": [
            { "id": "window-normal", "label": "Normal", "enabled": true },
            {
                "id": "window-move-workspace",
                "label": "Move to workspace",
                "enabled": false,
                "reason": "There is no other workspace"
            }
        ]
    }"#;

    /// A view over the shell's window menu: a row for each the shell offers, as it labels it,
    /// and the reason beside the ones this window cannot choose.
    const VIEW: &str = r#"app test.menu "Menu"

use desktop as desktop

view menu(m: desktop.WindowMenu) {
  stack {
    for item in m.rows {
      row {
        text(item.label)
        if not item.enabled {
          text(item.reason ?? "")
        }
      }
    }
  }
}
"#;

    /// The shell's description, as a host registers it for an app this runtime does not run: the
    /// menu's type, its rows, and where it stands.
    fn desktop() -> Meta {
        meta(json!({
            "app": "desktop",
            "types": [
                {"id": "WindowMenu", "name": "WindowMenu", "fields": [
                    {"id": "WindowMenu.tab", "name": "tab", "type": "Option<Text>"},
                    {"id": "WindowMenu.anchor", "name": "anchor", "type": "Option<Anchor>"},
                    {"id": "WindowMenu.rows", "name": "rows", "type": "List<WindowMenuRow>"},
                ]},
                {"id": "Anchor", "name": "Anchor", "fields": [
                    {"id": "Anchor.x", "name": "x", "type": "Float"},
                    {"id": "Anchor.y", "name": "y", "type": "Float"},
                ]},
                {"id": "WindowMenuRow", "name": "WindowMenuRow", "fields": [
                    {"id": "WindowMenuRow.id", "name": "id", "type": "Text"},
                    {"id": "WindowMenuRow.label", "name": "label", "type": "Text"},
                    {"id": "WindowMenuRow.enabled", "name": "enabled", "type": "Bool"},
                    {"id": "WindowMenuRow.reason", "name": "reason", "type": "Option<Text>"},
                ]},
                {"id": "MenuState", "name": "MenuState", "fields": [
                    {"id": "MenuState.state", "name": "state", "type": "enum PanelState"},
                ]},
            ],
            "enums": [
                {"id": "desktop#PanelState", "name": "PanelState", "doc": "", "variants": [
                    {"id": "PanelState.open", "name": "open", "doc": "", "fields": []},
                    {"id": "PanelState.closed", "name": "closed", "doc": "", "fields": []},
                ]},
            ],
        }))
    }

    /// A view over the shell's menu state: which case the shell published for it.
    const STATE_VIEW: &str = r#"app test.state "State"

use desktop as desktop

view state(m: desktop.MenuState) {
  stack {
    if m.state == .open {
      text("open")
    } else {
      text("closed")
    }
  }
}
"#;

    /// The shell's window menu, as a view of ours names it.
    fn menu() -> ResourceRef {
        ResourceRef::parse_id("window-menu", "desktop#WindowMenu").expect("a resource id")
    }

    /// The view, the shell's package loaded, and the shell's menu reading `text`.
    fn workspace(text: &str) -> (Runtime, ViewInstance) {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let shell = desktop();
        rt.load_foreign(shell.clone()).unwrap();
        let view = App::compile(VIEW, std::slice::from_ref(&shell));
        view.load(&rt);
        rt.set_reading(&menu(), text)
            .expect("a reading of the menu");
        let opened = rt
            .open_view(&me(), &view.view("menu"), vec![Value::resource(&menu())])
            .unwrap_or_else(|why| panic!("{why}"));
        (rt, opened)
    }

    /// The text of each row the view draws, in order.
    fn drawn(view: &ViewInstance) -> Vec<String> {
        fn texts(node: &UiNode, out: &mut Vec<String>) {
            if node.kind == "ui.Text"
                && let Some(Value::Text(text)) = node.props.get("value")
            {
                out.push(text.clone());
            }
            for child in &node.children {
                texts(child, out);
            }
        }
        let mut out = Vec::new();
        texts(view.tree(), &mut out);
        out
    }

    tilo_test_env::scenario! {
        fn a_reading_draws_its_rows_as_the_records_its_app_declares;

        phase a_view_draws_a_row_for_each_row_the_shell_offers;
        {
            let (_rt, view) = workspace(READING);
            assert_eq!(
                of_kind(view.tree(), "row").len(),
                2,
                "{}",
                dump(view.tree())
            );
            assert_eq!(
                drawn(&view),
                ["Normal", "Move to workspace", "There is no other workspace"],
                "each row as the shell labels it, with the reason of the one it cannot choose"
            );
        }

        phase a_new_reading_redraws_the_rows_it_lists;
        {
            let (rt, mut view) = workspace(READING);
            let closed = r#"{"tab": null, "anchor": null, "rows": []}"#;
            rt.set_reading(&menu(), closed).expect("a reading");
            view.poll_patches().unwrap();
            assert!(
                of_kind(view.tree(), "row").is_empty(),
                "{}",
                dump(view.tree())
            );
            assert_eq!(drawn(&view), Vec::<String>::new());
        }

        phase a_row_reads_as_the_record_its_app_declares;
        {
            let (rt, _view) = workspace(READING);
            let Value::List(rows) = rt.read(&menu(), "rows").unwrap() else {
                panic!("the menu's rows are a list");
            };
            assert_eq!(rows.len(), 2);
            let Value::Ref(row) = &rows[1] else {
                panic!("a row is a record: {:?}", rows[1]);
            };
            assert_eq!(row.target.ty, "desktop#WindowMenuRow");
            assert_eq!(row.target.id_string(), "window-move-workspace");
            assert_eq!(
                rt.read(&row.target, "label").unwrap(),
                Value::text("Move to workspace")
            );
            assert_eq!(rt.read(&row.target, "enabled").unwrap(), Value::Bool(false));
            assert_eq!(
                rt.read(&row.target, "reason").unwrap(),
                Value::some(Value::text("There is no other workspace"))
            );
            // A field the reading leaves out is the none its type declares.
            let Value::Ref(chosen) = &rows[0] else {
                panic!("a row is a record");
            };
            assert_eq!(rt.read(&chosen.target, "reason").unwrap(), Value::none());
        }

        phase a_case_in_a_reading_reads_as_the_case_its_app_declares;
        // The case a reading names is the case its app declares, so a view's `== .open` asks the
        // same of the reading as of a value of its own.
        {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let shell = desktop();
            rt.load_foreign(shell.clone()).unwrap();
            let view = App::compile(STATE_VIEW, std::slice::from_ref(&shell));
            view.load(&rt);
            let state =
                ResourceRef::parse_id("menu-state", "desktop#MenuState").expect("a resource id");
            rt.set_reading(&state, r#"{"state": "open"}"#)
                .expect("a reading of the menu's state");
            let opened = rt
                .open_view(&me(), &view.view("state"), vec![Value::resource(&state)])
                .unwrap_or_else(|why| panic!("{why}"));
            assert_eq!(
                drawn(&opened),
                ["open"],
                "the reading's case is the one the shell declares"
            );
        }

        phase a_row_reads_the_reading_as_it_is_now;
        {
            let (rt, _view) = workspace(READING);
            let Value::List(rows) = rt.read(&menu(), "rows").unwrap() else {
                panic!("the menu's rows are a list");
            };
            let Value::Ref(row) = &rows[0] else {
                panic!("a row is a record: {:?}", rows[0]);
            };
            // The shell moves the window: the row it named is still there, as the shell lists it now.
            let moved = r#"{"tab": null, "anchor": null, "rows": [
            { "id": "window-normal", "label": "Maximized", "enabled": false, "reason": "Full" },
            { "id": "window-move-workspace", "label": "Move to workspace", "enabled": false }
        ]}"#;
            rt.set_reading(&menu(), moved).expect("a reading");
            assert_eq!(
                rt.read(&row.target, "label").unwrap(),
                Value::text("Maximized")
            );
            assert_eq!(
                rt.read(&row.target, "reason").unwrap(),
                Value::some(Value::text("Full"))
            );
            // A row the reading no longer lists has no fields, rather than the old ones.
            let gone = r#"{"tab": null, "anchor": null, "rows": []}"#;
            rt.set_reading(&menu(), gone).expect("a reading");
            let why = rt.read(&row.target, "label").unwrap_err().to_string();
            assert!(why.contains("no reading of `window-normal`"), "{why}");
        }
    }

    tilo_test_env::scenario! {
        fn a_row_without_an_id_reads_by_where_it_stands;

        phase a_record_the_reading_names_no_id_for_reads_by_where_it_stands;
        {
            let (rt, _view) = workspace(READING);
            let Value::Opt(Some(anchor)) = rt.read(&menu(), "anchor").unwrap() else {
                panic!("the menu stands somewhere");
            };
            let Value::Ref(anchor) = *anchor else {
                panic!("the menu's anchor is a record");
            };
            assert_eq!(anchor.target.ty, "desktop#Anchor");
            assert_eq!(rt.read(&anchor.target, "x").unwrap(), Value::Float(40.0));
            assert_eq!(rt.read(&anchor.target, "y").unwrap(), Value::Float(50.0));
        }

        phase a_row_whose_id_is_empty_reads_by_where_it_stands;
        // A row that chooses nothing — the rule between two groups of rows — gives its id as
        // empty: it has no name of its own, so it reads by where it stands, as a record the
        // reading names no id for does.
        {
            let ruled = r#"{"tab": null, "anchor": null, "rows": [
            { "id": "window-normal", "label": "Normal", "enabled": true },
            { "id": "", "label": "", "enabled": false }
        ]}"#;
            let (rt, _view) = workspace(ruled);
            let Value::List(rows) = rt.read(&menu(), "rows").unwrap() else {
                panic!("the menu's rows are a list");
            };
            let Value::Ref(rule) = &rows[1] else {
                panic!("a row is a record: {:?}", rows[1]);
            };
            assert_eq!(rule.target.ty, "desktop#WindowMenuRow");
            assert_eq!(
                rt.read(&rule.target, "enabled").unwrap(),
                Value::Bool(false),
                "the rule reads its own fields"
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_reading_of_the_wrong_shape_is_an_error_or_a_fault;

        phase a_row_of_the_wrong_shape_is_an_error_naming_the_field;
        {
            let wrong = r#"{"tab": null, "anchor": null, "rows": [
            { "id": "window-normal", "label": "Normal", "enabled": "yes" }
        ]}"#;
            let why = opening(wrong).expect_err("a row whose enabled is text is no row");
            assert!(why.contains("`enabled` of `WindowMenuRow`"), "{why}");
            assert!(why.contains("\"yes\""), "{why}");
        }

        phase a_row_without_a_field_it_declares_is_an_error_not_an_empty_one;
        {
            let wrong = r#"{"tab": null, "anchor": null, "rows": [{ "id": "window-normal" }]}"#;
            let why = opening(wrong).expect_err("a row with no label is no row");
            assert!(why.contains("`label` of `WindowMenuRow`"), "{why}");
            assert!(why.contains("missing"), "{why}");
        }

        phase a_reading_that_is_no_menu_is_an_error_not_an_empty_one;
        {
            let why = opening("{ \"tab\": null, \"anchor\": null, \"rows\": 3 }")
                .expect_err("a menu's rows are a list");
            assert!(why.contains("`rows` of `window-menu`"), "{why}");
            assert!(why.contains("not a list"), "{why}");
        }

        phase a_type_its_loaded_app_does_not_declare_is_a_fault;
        // A type of an app whose package this runtime holds, which that package does not declare,
        // is a fault: no package can come with it.
        {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let elsewhere = meta(json!({
                "app": "desktop",
                "types": [{"id": "Elsewhere", "name": "Elsewhere", "fields": []}],
            }));
            rt.load_foreign(elsewhere).unwrap();
            let why = rt
                .read(&menu(), "title")
                .expect_err("the shell's package declares no menu");
            assert!(why.to_string().contains("is not declared"), "{why}");
            assert!(
                !rt.waiting(&why.to_string()),
                "nothing is waited for: {why}"
            );
            assert!(
                rt.take_wanted_packages().is_empty(),
                "no package is asked for"
            );
        }
    }

    /// A note made from another app's record, as the session's store keeps it: the link names
    /// the record, whose type is the other app's.
    const LINK_SOURCE: &str = r#"app test.cards "Cards"

type Board {
    notes: Collection<Note>
}

type Note {
    title: Text
}

root Board

default view board for Board

/// The board: its notes.
view board(b: Board) {
    list(b.notes) { n ->
        row(n.title)
    }
}

/// Adds a note.
action add_note(b: Board, title: Text) -> Note {
    b.notes.insert(Note(title: title))
}

/// What the note was made from, drawn as its app titles it.
view from(n: Note) {
    stack {
        text("note")
        if let made = created(n), f = made.from {
            text(meta(f).title)
        }
    }
}
"#;

    /// The shell's description with a menu a view can read the title of.
    fn titled_desktop() -> Meta {
        meta(json!({
            "app": "desktop",
            "types": [
                {"id": "WindowMenu", "name": "WindowMenu", "fields": [
                    {"id": "WindowMenu.title", "name": "title", "type": "Text",
                     "role": "title"},
                ]},
            ],
        }))
    }

    /// A note on a board made from the shell's menu, as a view over it would read what it was
    /// made from; the view and the note.
    fn note_from_the_shell(shell: &Meta) -> (Runtime, String, ResourceRef) {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let app = App::compile(LINK_SOURCE, std::slice::from_ref(shell));
        app.load(&rt);
        let board = rt.create("Board").expect("a board");
        let menu = menu();
        rt.set_reading(&menu, r#"{"title": "Book the van"}"#)
            .expect("a reading of the menu");
        let id = app.action("add_note");
        let made = rt
            .invoke_action_from(
                &me(),
                (
                    id.as_str(),
                    vec![Value::resource(&board), Value::text("Book it")],
                ),
                &menu,
            )
            .unwrap_or_else(|why| panic!("{why}"));
        let tilolang_runtime::InvokeOutcome::Done(Value::Ref(note)) = made else {
            panic!("a note was made: {made:?}");
        };
        (rt, app.view("from"), ResourceRef::clone(&note.target))
    }

    #[test]
    fn a_view_reading_an_apps_type_asks_for_that_apps_package() {
        let shell = titled_desktop();
        let (rt, view, note) = note_from_the_shell(&shell);
        let mut opened = rt
            .open_view(&me(), &view, vec![Value::resource(&note)])
            .unwrap_or_else(|why| panic!("the view waits, rather than failing: {why}"));
        assert!(
            rt.wants_packages(),
            "the view waits for the shell's package, not on a fault"
        );
        assert_eq!(
            rt.take_wanted_packages(),
            [("desktop".to_owned(), "desktop#WindowMenu".to_owned())],
            "the host is asked for the shell's package"
        );
        assert!(
            rt.take_wanted_packages().is_empty(),
            "an app is asked for once"
        );
        assert!(
            !of_kind(opened.tree(), "ui.Skeleton").is_empty(),
            "the view draws its waiting state: {}",
            dump(opened.tree())
        );

        // The host loads the shell's package: the view runs again and reads the menu's title.
        rt.load_foreign(shell).unwrap();
        opened.poll_patches().unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(
            drawn(&opened),
            ["note", "Book the van"],
            "the note reads what it was made from through the shell's own declaration"
        );
    }

    /// Opening the view over the shell's menu reading `text`, as it went.
    fn opening(text: &str) -> Result<(), String> {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let shell = desktop();
        rt.load_foreign(shell.clone()).map_err(|e| e.to_string())?;
        let view = App::compile(VIEW, std::slice::from_ref(&shell));
        view.load(&rt);
        rt.set_reading(&menu(), text).map_err(|e| e.to_string())?;
        rt.open_view(&me(), &view.view("menu"), vec![Value::resource(&menu())])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// An app whose remarks say who made each: nobody, someone only named, or a place in the
    /// paper with a note. Two of the cases carry values.
    fn papers() -> Meta {
        let carried = |id: &str, name: &str, ty: &str| json!({"id": id, "name": name, "type": ty, "merge": "register", "doc": ""});
        meta(json!({
            "app": "papers",
            "types": [
                {"id": "Paper", "name": "Paper", "fields": [
                    {"id": "Paper.remarks", "name": "remarks", "type": "List<Remark>"},
                ]},
                {"id": "Remark", "name": "Remark", "fields": [
                    {"id": "Remark.text", "name": "text", "type": "Text"},
                    {"id": "Remark.author", "name": "author", "type": "enum Author"},
                ]},
                {"id": "Spot", "name": "Spot", "fields": [
                    {"id": "Spot.page", "name": "page", "type": "Int"},
                ]},
                {"id": "Shelf", "name": "Shelf", "fields": [
                    {"id": "Shelf.tags", "name": "tags", "type": "Map<Text>"},
                    {"id": "Shelf.counts", "name": "counts", "type": "Map<Int>"},
                    {"id": "Shelf.spots", "name": "spots", "type": "Map<Spot>"},
                    {"id": "Shelf.drawers", "name": "drawers", "type": "Map<Map<Int>>"},
                ]},
            ],
            "enums": [
                {"id": "papers#Author", "name": "Author", "doc": "", "variants": [
                    {"id": "Author.nobody", "name": "nobody", "doc": "", "fields": []},
                    {"id": "Author.named", "name": "named", "doc": "", "fields": [
                        carried("Author.named.value", "value", "Text"),
                    ]},
                    {"id": "Author.placed", "name": "placed", "doc": "", "fields": [
                        carried("Author.placed.spot", "spot", "Spot"),
                        carried("Author.placed.note", "note", "Option<Text>"),
                    ]},
                ]},
            ],
        }))
    }

    /// A view over a paper's remarks: who made each, with what that case carries.
    const REMARKS_VIEW: &str = r#"app test.remarks "Remarks"

use papers as papers

view remarks(p: papers.Paper) {
  stack {
    for r in p.remarks {
      match r.author {
        .nobody => text("nobody"),
        .named(name) => text(name),
        .placed(spot, note) => {
          text("page {spot.page}")
          text(note ?? "no note")
        }
      }
    }
  }
}
"#;

    /// The paper a view of ours names.
    fn paper() -> ResourceRef {
        ResourceRef::parse_id("paper-1", "papers#Paper").expect("a resource id")
    }

    /// A runtime that knows the papers' app and holds `reading` of the paper.
    fn with_paper(reading: &str) -> Result<Runtime, String> {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        rt.load_foreign(papers()).map_err(|e| e.to_string())?;
        rt.set_reading(&paper(), reading)
            .map_err(|e| e.to_string())?;
        Ok(rt)
    }

    #[test]
    fn a_case_that_carries_values_reads_from_a_reading_with_each_value_as_its_type() {
        // Each case as a data enum is written in JSON: its name under `$variant` beside the
        // values it carries, by name. A plain case is its name, alone or under that key.
        let reading = r#"{"remarks": [
            {"text": "one", "author": "nobody"},
            {"text": "two", "author": {"$variant": "named", "value": "Ada"}},
            {"text": "three", "author":
                {"$variant": "placed", "spot": {"page": 4}, "note": "in the margin"}},
            {"text": "four", "author": {"$variant": "placed", "spot": {"page": 9}}},
            {"text": "five", "author": {"$variant": "nobody"}}
        ]}"#;
        let rt = with_paper(reading).unwrap_or_else(|why| panic!("{why}"));
        let view = App::compile(REMARKS_VIEW, std::slice::from_ref(&papers()));
        view.load(&rt);
        let opened = rt
            .open_view(
                &me(),
                &view.view("remarks"),
                vec![Value::resource(&paper())],
            )
            .unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(
            drawn(&opened),
            [
                "nobody",
                "Ada",
                "page 4",
                "in the margin",
                "page 9",
                "no note",
                "nobody"
            ],
            "each case with what it carries, in the order of the remarks: {}",
            dump(opened.tree())
        );

        // The value a case carries is of the type the case declares for it.
        let Value::List(remarks) = rt.read(&paper(), "remarks").unwrap() else {
            panic!("the remarks are a list");
        };
        let Value::Ref(second) = &remarks[1] else {
            panic!("a remark is a resource: {:?}", remarks[1]);
        };
        let Value::Enum(author) = rt.read(&second.target, "author").unwrap() else {
            panic!("an author is a case");
        };
        assert_eq!(author.ty, "papers#Author");
        assert_eq!(author.variant, "Author.named");
        assert_eq!(
            author.fields.get("Author.named.value"),
            Some(&Value::text("Ada"))
        );
        let Value::Ref(third) = &remarks[2] else {
            panic!("a remark is a resource: {:?}", remarks[2]);
        };
        let Value::Enum(placed) = rt.read(&third.target, "author").unwrap() else {
            panic!("an author is a case");
        };
        let Some(Value::Ref(spot)) = placed.fields.get("Author.placed.spot") else {
            panic!("what the case carries is a resource: {placed:?}");
        };
        assert_eq!(spot.target.ty, "papers#Spot");
        assert_eq!(rt.read(&spot.target, "page").unwrap(), Value::Int(4));
    }

    #[test]
    fn a_case_a_reading_spells_wrongly_is_a_fault_that_says_which() {
        // A case the enum does not declare.
        let rt = with_paper(r#"{"remarks": [{"text": "x", "author": {"$variant": "gone"}}]}"#)
            .unwrap_or_else(|why| panic!("{why}"));
        let why = rt
            .read(&paper(), "remarks")
            .expect_err("a case the enum does not declare")
            .to_string();
        assert!(why.contains("`gone` is not a case of `Author`"), "{why}");

        // A value of another type than the case declares for it.
        let rt = with_paper(
            r#"{"remarks": [{"text": "x", "author": {"$variant": "named", "value": 7}}]}"#,
        )
        .unwrap_or_else(|why| panic!("{why}"));
        let why = rt
            .read(&paper(), "remarks")
            .expect_err("a number where the case carries a text")
            .to_string();
        assert!(
            why.contains("`value` of `.named`") && why.contains("is not a Text"),
            "{why}"
        );

        // A value the case declares and the reading leaves out, which is no option.
        let rt = with_paper(r#"{"remarks": [{"text": "x", "author": {"$variant": "named"}}]}"#)
            .unwrap_or_else(|why| panic!("{why}"));
        let why = rt
            .read(&paper(), "remarks")
            .expect_err("a case without the value it carries")
            .to_string();
        assert!(
            why.contains("`value` of `.named` is missing from the reading"),
            "{why}"
        );
    }

    /// A view over a shelf's maps: each tag by its key, in the keys' order, a count by its key,
    /// the page of a spot a map holds, and what a map inside a map holds.
    const SHELF_VIEW: &str = r#"app test.shelf "Shelf"

use papers as papers

view shelf(s: papers.Shelf) {
  stack {
    for k in s.tags.keys() {
      text(k)
      let tag = match s.tags.get(k) {
        .some(v) => v,
        .none => "none",
      }
      text(tag)
    }
    let b = match s.counts.get("b") {
      .some(v) => v,
      .none => 0,
    }
    text("{b}")
    match s.spots.get("first") {
      .some(spot) => text("page {spot.page}"),
      .none => text("no spot"),
    }
    match s.drawers.get("top") {
      .none => text("no drawer"),
      .some(drawer) => {
        for k in drawer.keys() {
          text(k)
        }
        let pens = match drawer.get("pens") {
          .some(v) => v,
          .none => 0,
        }
        text("{pens}")
      }
    }
  }
}
"#;

    /// The shelf a view of ours names.
    fn shelf() -> ResourceRef {
        ResourceRef::parse_id("shelf-1", "papers#Shelf").expect("a resource id")
    }

    #[test]
    fn a_map_in_a_reading_reads_by_its_keys_each_value_as_its_type() {
        // A map as the object of its entries, written in no order.
        const SHELF: &str = r#"{"tags": {"b": "blue", "a": "amber"}, "counts": {"a": 1, "b": 2},
            "spots": {"first": {"page": 3}},
            "drawers": {"top": {"pens": 4, "clips": 1}}}"#;
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        rt.load_foreign(papers()).unwrap();
        rt.set_reading(&shelf(), SHELF)
            .expect("a reading of the shelf");
        let view = App::compile(SHELF_VIEW, std::slice::from_ref(&papers()));
        view.load(&rt);
        let mut opened = rt
            .open_view(&me(), &view.view("shelf"), vec![Value::resource(&shelf())])
            .unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(
            drawn(&opened),
            [
                "a", "amber", "b", "blue", "2", "page 3", "clips", "pens", "4"
            ],
            "each entry by its key, in the keys' order: {}",
            dump(opened.tree())
        );

        // The app writes: a tag more, another count, and two maps emptied. The view that
        // asked the maps for their keys and their values draws again over the new reading.
        rt.set_reading(
            &shelf(),
            r#"{"tags": {"b": "blue", "a": "amber", "c": "coral"}, "counts": {"b": 5},
                "spots": {}, "drawers": {}}"#,
        )
        .expect("a reading of the shelf");
        opened.poll_patches().unwrap();
        assert_eq!(
            drawn(&opened),
            [
                "a",
                "amber",
                "b",
                "blue",
                "c",
                "coral",
                "5",
                "no spot",
                "no drawer"
            ],
            "the view follows the maps it asked: {}",
            dump(opened.tree())
        );
        rt.set_reading(&shelf(), SHELF)
            .expect("a reading of the shelf");

        // A field of a map reads as one does here: its entries, each a key and its value.
        assert_eq!(
            rt.read(&shelf(), "counts").unwrap(),
            Value::List(vec![
                Value::Tuple(vec![Value::text("a"), Value::Int(1)]),
                Value::Tuple(vec![Value::text("b"), Value::Int(2)]),
            ])
        );
        // What a map holds is of the type the map declares: a resource a view reads the
        // fields of, and a map inside a map a map a view asks the same way.
        let Value::List(spots) = rt.read(&shelf(), "spots").unwrap() else {
            panic!("a map reads as its entries");
        };
        let Value::Tuple(first) = &spots[0] else {
            panic!("an entry is a key and a value: {:?}", spots[0]);
        };
        let Value::Ref(spot) = &first[1] else {
            panic!("what the map holds is a resource: {:?}", first[1]);
        };
        assert_eq!(spot.target.ty, "papers#Spot");
        assert_eq!(rt.read(&spot.target, "page").unwrap(), Value::Int(3));
        assert_eq!(
            rt.read(&shelf(), "drawers").unwrap(),
            Value::List(vec![Value::Tuple(vec![
                Value::text("top"),
                Value::Map(vec![
                    (Value::text("clips"), Value::Int(1)),
                    (Value::text("pens"), Value::Int(4)),
                ]),
            ])])
        );

        // A value of another type than the map declares, and a field that holds no map, are
        // faults that say which.
        rt.set_reading(&shelf(), r#"{"tags": {"a": 7}}"#)
            .expect("a reading");
        let why = rt
            .read(&shelf(), "tags")
            .expect_err("a number in a map of texts")
            .to_string();
        assert!(
            why.contains("the entry `a`") && why.contains("is not a Text"),
            "{why}"
        );
        rt.set_reading(&shelf(), r#"{"tags": [1]}"#)
            .expect("a reading");
        let why = rt
            .read(&shelf(), "tags")
            .expect_err("a list where a map stands")
            .to_string();
        assert!(why.contains("is not a map"), "{why}");
    }

    #[test]
    fn a_map_the_runtime_itself_encoded_reads_as_it_did() {
        // The entries as a list of pairs in the runtime's own encoding, and none.
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        rt.load_foreign(papers()).unwrap();
        rt.set_reading(
            &shelf(),
            r#"{"counts": {"list": [{"tuple": ["a", 1]}]}, "tags": {"list": []}}"#,
        )
        .expect("a reading of the shelf");
        assert_eq!(
            rt.read(&shelf(), "counts").unwrap(),
            Value::List(vec![Value::Tuple(vec![Value::text("a"), Value::Int(1)])])
        );
        assert_eq!(rt.read(&shelf(), "tags").unwrap(), Value::List(Vec::new()));
    }

    /// A view over a note of an app this runtime does not run, handing the note's reactions to
    /// the strip a person presses.
    const NOTE_VIEW: &str = r#"app test.note "Note"

use other as other

view note(n: other.Note) {
  ui.Reactions(reactions: n.reactions)
}
"#;

    /// The app whose notes this view reads: what a note says, and who reacted to it by emoji.
    fn reacted_notes() -> Meta {
        meta(json!({
            "app": "other",
            "types": [
                {"id": "Note", "name": "Note", "fields": [
                    {"id": "Note.text", "name": "text", "type": "Text"},
                    {"id": "Note.reactions", "name": "reactions", "type": "Map<Set<Actor>>"},
                ]},
            ],
        }))
    }

    #[test]
    fn a_map_field_a_reading_leaves_out_reaches_a_view_as_the_empty_map() {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let other = reacted_notes();
        rt.load_foreign(other.clone()).unwrap();
        let view = App::compile(NOTE_VIEW, std::slice::from_ref(&other));
        view.load(&rt);
        let note = ResourceRef::parse_id("note-1", "other#Note").expect("a resource id");
        for reading in [
            // A note written before its app kept reactions, and one whose map is empty: the
            // strip is handed the map it declares, not a list the shell would refuse.
            r#"{"text": "Groceries"}"#,
            r#"{"text": "Groceries", "reactions": {}}"#,
        ] {
            rt.set_reading(&note, reading)
                .expect("a reading of the note");
            let opened = rt
                .open_view(&me(), &view.view("note"), vec![Value::resource(&note)])
                .unwrap_or_else(|why| panic!("{why}"));
            let strip = of_kind(opened.tree(), "ui.Reactions")[0].clone();
            assert_eq!(
                strip.props.get("reactions"),
                Some(&Value::Map(Vec::new())),
                "a note that holds no reaction hands the strip an empty map ({reading}): {}",
                dump(opened.tree())
            );
        }
    }

    /// An app that lists its records rather than running here (a workspace's invocations): each
    /// asks someone something, waits or runs, and its filters are the defaults of its fields.
    fn listed() -> Meta {
        let mut listed = meta(json!({
            "app": "tilo.listed",
            "types": [
                {"id": "tilo.listed#Ask", "name": "Ask", "fields": [
                    {"id": "Ask.words", "name": "words", "type": "Text"},
                    {"id": "Ask.status", "name": "status", "type": "enum Status"},
                    {"id": "Ask.person", "name": "person", "type": "Option<Actor>"},
                ]},
            ],
            "enums": [
                {"id": "tilo.listed#Status", "name": "Status", "doc": "", "variants": [
                    {"id": "Status.waiting", "name": "waiting", "doc": "", "fields": []},
                    {"id": "Status.running", "name": "running", "doc": "", "fields": []},
                ]},
            ],
        }));
        tilolang::search::describe_defaults(&mut listed);
        listed
    }

    /// What is asked of the person looking: the app's records that wait for them, read from its
    /// listing with no resource to look in.
    const ASKING_VIEW: &str = r#"app test.asking "Asking"

use tilo.listed as listed

view asking() {
  stack {
    for ask in query(listed.Ask, status: .waiting, person: me) {
      row { text(ask.words) }
    }
  }
}
"#;

    /// The app's listing, `asks` its records.
    fn listing(asks: serde_json::Value) -> String {
        json!({ "items": asks }).to_string()
    }

    tilo_test_env::scenario! {
        fn a_view_lists_another_apps_records_and_follows_them;

        phase what_waits_for_the_person_is_listed_once_its_listing_comes;
        {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let app = listed();
            assert!(
                app.types[0].search.filters.iter().any(|f| f.key == "status"),
                "a listed type is filtered by its fields: {:?}",
                app.types[0].search
            );
            rt.load_foreign(app.clone()).unwrap();
            let view = App::compile(ASKING_VIEW, std::slice::from_ref(&app));
            view.load(&rt);
            let mut opened = rt
                .open_view(&me(), &view.view("asking"), Vec::new())
                .unwrap_or_else(|why| panic!("{why}"));
            assert_eq!(drawn(&opened), Vec::<String>::new(), "nothing is listed yet");
            let wanted = rt.take_wanted();
            assert_eq!(
                wanted,
                [("*".to_owned(), "tilo.listed#Ask".to_owned())],
                "the host is asked for the app's listing, once"
            );
            let all = tilolang_runtime::foreign::listing_of("tilo.listed#Ask");
            rt.set_reading(
                &all,
                &listing(json!([
                    {"id": "a1", "words": "Send the report?", "status": "waiting", "person": "me"},
                    {"id": "a2", "words": "Book the room?", "status": "running", "person": "me"},
                    {"id": "a3", "words": "Pay the invoice?", "status": "waiting", "person": "you"},
                    {"id": "a4", "words": "Read the folder", "status": "running"},
                ])),
            )
            .expect("a listing");
            opened.poll_patches().unwrap();
            assert_eq!(
                drawn(&opened),
                ["Send the report?"],
                "only what waits for the person looking: {}",
                dump(opened.tree())
            );

            // A record that stops waiting leaves, and a new one comes.
            rt.set_reading(
                &all,
                &listing(json!([
                    {"id": "a1", "words": "Send the report?", "status": "running", "person": "me"},
                    {"id": "a5", "words": "Delete the draft?", "status": "waiting", "person": "me"},
                ])),
            )
            .expect("a listing");
            opened.poll_patches().unwrap();
            assert_eq!(drawn(&opened), ["Delete the draft?"]);
            assert!(rt.take_wanted().is_empty(), "the listing is asked for once");

            // Each listed record is the app's own, by its id: what answering it names.
            let ask = ResourceRef::parse_id("a5", "tilo.listed#Ask").expect("an id");
            assert_eq!(rt.read(&ask, "words").unwrap(), Value::text("Delete the draft?"));
        }
    }
}
