//! A view over another app's reading whose fields are not bare values: the window menu's rows,
//! a list of objects the desk offers a window. Each row reads as the record its app declares —
//! its label, whether it can be chosen and why not — and a reading that does not match the
//! declared types is an error that says which field of which row.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::json;
    use tilolang_runtime::{
        AmStore, Meta, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
    };

    /// The window menu as the desk publishes it: the window it is for, where it stands, and the
    /// rows that window offers now (`crates/session/window-menu`, `session/src/mount/shell.rs`).
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

    /// A view over the desk's window menu: a row for each the desk offers, as it labels it,
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

    /// The desk's description, as a host registers it for an app this runtime does not run: the
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

    /// A view over the desk's menu state: which case the desk published for it.
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

    /// The desk's window menu, as a view of ours names it.
    fn menu() -> ResourceRef {
        ResourceRef::parse_id("window-menu", "desktop#WindowMenu").expect("a resource id")
    }

    /// The view, the desk's package loaded, and the desk's menu reading `text`.
    fn workspace(text: &str) -> (Runtime, ViewInstance) {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let desk = desktop();
        rt.load_foreign(desk.clone()).unwrap();
        let view = App::compile(VIEW, std::slice::from_ref(&desk));
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

    #[test]
    fn a_view_draws_a_row_for_each_row_the_desk_offers() {
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
            "each row as the desk labels it, with the reason of the one it cannot choose"
        );
    }

    #[test]
    fn a_new_reading_redraws_the_rows_it_lists() {
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

    #[test]
    fn a_row_reads_as_the_record_its_app_declares() {
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

    /// The case a reading names is the case its app declares, so a view's `== .open` asks the
    /// same of the reading as of a value of its own.
    #[test]
    fn a_case_in_a_reading_reads_as_the_case_its_app_declares() {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let desk = desktop();
        rt.load_foreign(desk.clone()).unwrap();
        let view = App::compile(STATE_VIEW, std::slice::from_ref(&desk));
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
            "the reading's case is the one the desk declares"
        );
    }

    #[test]
    fn a_row_reads_the_reading_as_it_is_now() {
        let (rt, _view) = workspace(READING);
        let Value::List(rows) = rt.read(&menu(), "rows").unwrap() else {
            panic!("the menu's rows are a list");
        };
        let Value::Ref(row) = &rows[0] else {
            panic!("a row is a record: {:?}", rows[0]);
        };
        // The desk moves the window: the row it named is still there, as the desk lists it now.
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

    #[test]
    fn a_record_the_reading_names_no_id_for_reads_by_where_it_stands() {
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

    /// A row that chooses nothing — the rule between two groups of rows — gives its id as
    /// empty: it has no name of its own, so it reads by where it stands, as a record the
    /// reading names no id for does.
    #[test]
    fn a_row_whose_id_is_empty_reads_by_where_it_stands() {
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

    #[test]
    fn a_row_of_the_wrong_shape_is_an_error_naming_the_field() {
        let wrong = r#"{"tab": null, "anchor": null, "rows": [
            { "id": "window-normal", "label": "Normal", "enabled": "yes" }
        ]}"#;
        let why = opening(wrong).expect_err("a row whose enabled is text is no row");
        assert!(why.contains("`enabled` of `WindowMenuRow`"), "{why}");
        assert!(why.contains("\"yes\""), "{why}");
    }

    #[test]
    fn a_row_without_a_field_it_declares_is_an_error_not_an_empty_one() {
        let wrong = r#"{"tab": null, "anchor": null, "rows": [{ "id": "window-normal" }]}"#;
        let why = opening(wrong).expect_err("a row with no label is no row");
        assert!(why.contains("`label` of `WindowMenuRow`"), "{why}");
        assert!(why.contains("missing"), "{why}");
    }

    #[test]
    fn a_reading_that_is_no_menu_is_an_error_not_an_empty_one() {
        let why = opening("{ \"tab\": null, \"anchor\": null, \"rows\": 3 }")
            .expect_err("a menu's rows are a list");
        assert!(why.contains("`rows` of `window-menu`"), "{why}");
        assert!(why.contains("not a list"), "{why}");
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

    /// The desk's description with a menu a view can read the title of.
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

    /// A note on a board made from the desk's menu, as a view over it would read what it was
    /// made from; the view and the note.
    fn note_from_the_desk(desk: &Meta) -> (Runtime, String, ResourceRef) {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let app = App::compile(LINK_SOURCE, std::slice::from_ref(desk));
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
        let desk = titled_desktop();
        let (rt, view, note) = note_from_the_desk(&desk);
        let mut opened = rt
            .open_view(&me(), &view, vec![Value::resource(&note)])
            .unwrap_or_else(|why| panic!("the view waits, rather than failing: {why}"));
        assert!(
            rt.wants_packages(),
            "the view waits for the desk's package, not on a fault"
        );
        assert_eq!(
            rt.take_wanted_packages(),
            [("desktop".to_owned(), "desktop#WindowMenu".to_owned())],
            "the host is asked for the desk's package"
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

        // The host loads the desk's package: the view runs again and reads the menu's title.
        rt.load_foreign(desk).unwrap();
        opened.poll_patches().unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(
            drawn(&opened),
            ["note", "Book the van"],
            "the note reads what it was made from through the desk's own declaration"
        );
    }

    /// A type of an app whose package this runtime holds, which that package does not declare,
    /// is a fault: no package can come with it.
    #[test]
    fn a_type_its_loaded_app_does_not_declare_is_a_fault() {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let elsewhere = meta(json!({
            "app": "desktop",
            "types": [{"id": "Elsewhere", "name": "Elsewhere", "fields": []}],
        }));
        rt.load_foreign(elsewhere).unwrap();
        let why = rt
            .read(&menu(), "title")
            .expect_err("the desk's package declares no menu");
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

    /// Opening the view over the desk's menu reading `text`, as it went.
    fn opening(text: &str) -> Result<(), String> {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let desk = desktop();
        rt.load_foreign(desk.clone()).map_err(|e| e.to_string())?;
        let view = App::compile(VIEW, std::slice::from_ref(&desk));
        view.load(&rt);
        rt.set_reading(&menu(), text).map_err(|e| e.to_string())?;
        rt.open_view(&me(), &view.view("menu"), vec![Value::resource(&menu())])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
