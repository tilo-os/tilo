//! What declarations tell the platform (labels, roles,
//! icons, field roles, row views, scenarios), and the errors of the native vocabulary.

use crate::common::{codes, compile_body};
use tilolang::meta::PackageMeta;

fn expect(code: &str, body: &str) {
    let got = codes(body);
    assert!(
        got.iter().any(|c| c == code),
        "expected {code} for:\n{body}\ngot {got:?}"
    );
}

fn ok(body: &str) {
    let got = codes(body);
    assert!(
        got.is_empty(),
        "expected no errors for:\n{body}\ngot {got:?}"
    );
}

const NOTES: &str = r#"type Note {
    title: Text
    body: Text
    pinned: Bool = false
}

type Notebook {
    notes: Collection<Note>
}

/// Which notes a notebook lists.
enum NoteFilter {
    /// All notes
    all
    /// Pinned.
    pinned
    recently_changed
}

root Notebook

default view notebook for Notebook

row view note_row for Note

/// Start a new note in the notebook.
primary action new_note(nb: Notebook) -> Note (icon: .plus) {
    return nb.notes.insert(Note(title: ""))
}

/// Pin or unpin the note so it stays on top.
action toggle_pin(n: Note, pinned: Bool = not n.pinned) (icon: .pin) {
    n.pinned = pinned
}

/// Archive.
action archive_everything_now(nb: Notebook) {
    for n in nb.notes {
        nb.notes.remove(n)
    }
}

destructive action delete(nb: Notebook, n: Note) (icon: .trash_2, label: "Delete note") {
    nb.notes.remove(n)
}

action more(n: Note) (icon: .more_horizontal) {
    n.pinned = true
}

view note_row(n: Note) {
    row(n.title, secondary: n.body, mark: if n.pinned { .pin })
}

view notebook(nb: Notebook) {
    state opened: Option<Note> = .none
    std.list(nb.notes, opened) { (n, at) ->
        ui.ListItem(item: n, presence: at) {
            note_row(n)
        }
    }
}

scenario "a week of notes" {
    let nb = root Notebook
    let n = new_note(nb)
    n.title = "Q3 plan"
    toggle_pin(n)
}"#;

fn notes() -> tilolang::Compiled {
    let src = format!("app so.tilo.notes \"Notes\" (icon: .bookmark)\n{NOTES}\n");
    crate::common::compile(&src)
}

fn action<'a>(m: &'a PackageMeta, name: &str) -> &'a tilolang::meta::ActionMeta {
    m.actions
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("no action {name}"))
}

// USER REQUIREMENT (2026-10-20): "An action's label is its name in
// sentence case (`new_note` -> "New note"), or its doc comment's first line when that is
// shorter. `icon:` and `role:` are declared on the action, not on each button."
#[test]
fn actions_carry_their_label_icon_and_role() {
    let m = notes().meta;
    let new_note = action(&m, "new_note");
    assert_eq!(new_note.label, "New note", "the doc line is longer");
    assert_eq!(new_note.icon.as_deref(), Some("plus"));
    assert_eq!(new_note.role.as_deref(), Some("primary"));
    assert!(!new_note.scenario);
    let pin = action(&m, "toggle_pin");
    assert_eq!(pin.label, "Toggle pin");
    assert_eq!(pin.role, None);
    assert_eq!(
        action(&m, "archive_everything_now").label,
        "Archive",
        "a shorter first doc line wins, without its period"
    );
    let delete = action(&m, "delete");
    assert_eq!(delete.label, "Delete note", "`label:` overrides");
    assert_eq!(delete.role.as_deref(), Some("destructive"));
    assert_eq!(delete.icon.as_deref(), Some("trash-2"));
    assert_eq!(
        action(&m, "more").icon.as_deref(),
        Some("more-horizontal"),
        "icons travel in kebab case"
    );
}

#[test]
fn fields_variants_and_the_app_carry_what_they_are() {
    let m = notes().meta;
    assert_eq!(m.icon.as_deref(), Some("bookmark"));
    let note = m.types.iter().find(|t| t.name == "Note").expect("Note");
    let names: Vec<&str> = note.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["title", "body", "pinned"]);
    let filter = m
        .enums
        .iter()
        .find(|e| e.name == "NoteFilter")
        .expect("enum");
    let labels: Vec<&str> = filter.variants.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(labels, vec!["All notes", "Pinned", "Recently changed"]);
}

// The cases of a filter or a choice show an icon beside their label: `(icon: .x)` after the
// variant, validated like an action's.
#[test]
fn enum_variants_carry_their_icon() {
    let c = compile_body(
        "/// Which notes show.\nenum NoteFilter {\n    /// All notes\n    all (icon: .file_text)\n    pinned (icon: .pin)\n    shared (icon: .users)\n    recent\n}",
    );
    let e = c
        .meta
        .enums
        .iter()
        .find(|e| e.name == "NoteFilter")
        .expect("enum");
    let icons: Vec<Option<&str>> = e.variants.iter().map(|v| v.icon.as_deref()).collect();
    assert_eq!(
        icons,
        vec![Some("file-text"), Some("pin"), Some("users"), None],
        "icons travel in kebab case"
    );
    assert_eq!(
        e.variants[0].label, "All notes",
        "the doc still gives the label"
    );
    let json = String::from_utf8(c.meta.to_json_bytes()).expect("utf8");
    assert_eq!(
        json.matches("\"icon\"").count(),
        3,
        "a variant without an icon writes none: {json}"
    );
    let imported = crate::common::compile_with(
        "app t.y \"Y\"\nuse t.x as x\nview v(f: x.NoteFilter) {\n    text(\"f\")\n}\n",
        std::slice::from_ref(&c.meta),
    );
    assert!(imported.is_ok(), "{:?}", imported.err());
}

const PIN: &str = "type Note {\n    title: Text\n    pinned: Bool = false\n}\n/// Pin the note.\naction toggle_pin(n: Note, pinned: Bool = not n.pinned) (label: if n.pinned { \"Unpin\" } else { \"Pin\" }, icon: if n.pinned { .bell_off } else { .pin }, on: n.pinned) {\n    n.pinned = pinned\n}";

// An action's label and icon may follow its subject, and `on:` says it is pressed: the parts
// that depend on the subject become a hidden view of it, one `present` node; the static label
// and icon are the `else` branch, what a menu shows without evaluating anything.
#[test]
fn an_action_presents_its_state_through_a_hidden_view() {
    let c = compile_body(PIN);
    let pin = action(&c.meta, "toggle_pin");
    assert_eq!(pin.label, "Pin", "the else branch is the static label");
    assert_eq!(pin.icon.as_deref(), Some("pin"));
    let id = c.ids.get("toggle_pin~present").expect("an id in the lock");
    assert_eq!(pin.present.as_deref(), Some(id));
    let view = c
        .meta
        .views
        .iter()
        .find(|v| v.id == id)
        .expect("the view is in the metadata, for the host to run");
    assert_eq!(view.presents.as_deref(), Some(pin.id.as_str()));
    assert_eq!(view.params.len(), 1, "it takes the subject");
    assert_eq!(view.params[0].name, "n");
    assert!(
        c.meta.offered_views().all(|v| v.id != id),
        "it is never offered as a view"
    );
    assert!(c.meta.default_views.is_empty() && c.meta.row_views.is_empty());
    let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has(&format!("v_{id}")), "the view is exported");
    assert!(has("present"), "its node is a `present` node");
    crate::common::validate(&c.wasm);
    // A package using this one does not see it as a view.
    let other = crate::common::compile_with(
        "app t.y \"Y\"\nuse t.x as x\nview v(n: x.Note) {\n    text(n.title)\n}\n",
        std::slice::from_ref(&c.meta),
    );
    assert!(other.is_ok(), "{:?}", other.err());
    // Static options need no view.
    let plain = compile_body(
        "type T {\n    n: Int\n}\naction a(x: T) (label: \"Go\", icon: .plus) {\n    x.n = 1\n}",
    );
    assert!(plain.meta.views.is_empty() && plain.meta.actions[0].present.is_none());
    let printed = tilolang::print_module(
        &tilolang::parse(&format!("app t.x \"X\"\n{PIN}\n")).expect("parses"),
    );
    assert!(
        printed.contains(
            "action toggle_pin(n: Note, pinned: Bool = not n.pinned) (\n    label: if n.pinned { \"Unpin\" } else { \"Pin\" },\n    icon: if n.pinned { .bell_off } else { .pin },\n    on: n.pinned,\n) {"
        ),
        "{printed}"
    );
}

#[test]
fn a_presented_state_is_typed_and_reads_only_the_subject() {
    let head = "type Note {\n    title: Text\n    pinned: Bool = false\n    n: Int\n}\n";
    let act = |opts: &str| format!("{head}action a(x: Note, y: Note) ({opts}) {{\n    x.n = 1\n}}");
    expect("E020", &act("on: x.n"));
    expect("E020", &act("label: if x.pinned { 1 } else { 2 }"));
    expect("E097", &act("icon: if x.pinned { \"pin\" } else { .pin }"));
    expect("E091", &act("icon: if x.pinned { .pinn } else { .pin }"));
    expect("E090", &act("on: y.pinned"));
    expect("E090", &act("label: if y.pinned { \"a\" } else { \"b\" }"));
    expect("E049", &act("on: invoker == invoker"));
    expect("E090", "action a() (on: true) {\n    return\n}");
    expect("E090", &act("label: x.title"));
    ok(&act(
        "label: if x.pinned { \"Unpin {x.title}\" } else if x.n > 1 { \"Pin\" } else { \"Pin it\" }, on: x.pinned and x.n > 0",
    ));
    let c = compile_body(&act(
        "label: if x.pinned { \"A\" } else if x.n > 1 { \"B\" } else { \"C\" }",
    ));
    assert_eq!(
        c.meta.actions[0].label, "C",
        "the last else is the static label"
    );
}

#[test]
fn a_view_of_one_item_draws_that_items_row() {
    // The row is the view's whole body, so the parameters after the item are what the place
    // around the row says: a list grouped by a day gives the day, and the row leaves it out.
    ok(r#"type Note {
    title: Text
    pinned: Bool = false
}

type Notebook {
    notes: Collection<Note>
}

root Notebook

default view notebook for Notebook

row view note_row for Note

view note_row(n: Note) {
    row(n.title)
}

view note_in_place(n: Note, pinned: Bool) {
    let secondary: Option<Text> = if pinned { "Pinned" } else { .none }
    row(n.title, secondary: secondary)
}

view notebook(nb: Notebook) {
    list(nb.notes) { n ->
        note_in_place(n, n.pinned)
    }
}
"#);
}

#[test]
fn a_view_of_one_item_that_draws_more_than_its_row_is_no_row() {
    expect(
        "E097",
        r"type Note {
    title: Text
}

type Notebook {
    notes: Collection<Note>
}

root Notebook

default view notebook for Notebook

view note_layout(n: Note) {
    section(n.title) {
        row(n.title)
    }
}

view notebook(nb: Notebook) {
    list(nb.notes) { n ->
        note_layout(n)
    }
}",
    );
}

#[test]
fn row_views_are_in_the_metadata() {
    let c = notes();
    let id = |n: &str| c.ids.get(n).expect("id").to_string();
    assert_eq!(c.meta.row_view(&id("Note")), Some(id("note_row").as_str()));
    assert_eq!(c.meta.row_view(&id("Notebook")), None);
}

/// A type whose window switches between views names them, in the order the bar shows them; a
/// view it does not name is a helper its own views place and no window offers.
#[test]
fn the_views_a_window_switches_between_are_declared() {
    let c = crate::common::compile_body(
        r#"type Note {
    title: Text
}

root Notebook

type Notebook {
    notes: Collection<Note>
}

default view note_cards for Notebook

view note_cards(nb: Notebook) {
    std.list(nb.notes) { (n, at) ->
        ui.ListItem(item: n, presence: at)
    }
}

view note_board(nb: Notebook) {
    std.page(nb) {
        text("Board")
    }
}

view notebook_helper(nb: Notebook) {
    std.header()
}

views note_cards, note_board for Notebook"#,
    );
    let id = |n: &str| c.ids.get(n).expect("id").to_string();
    assert_eq!(
        c.meta.views_of(&id("Notebook")),
        vec![id("note_cards"), id("note_board")],
        "in the order declared, the default view first"
    );
    // Without the declaration, the type's views are all helpers and no bar switches them.
    let plain = crate::common::compile_body(
        r#"type Note {
    title: Text
}

root Notebook

type Notebook {
    notes: Collection<Note>
}

default view note_cards for Notebook

view note_cards(nb: Notebook) {
    std.list(nb.notes) { (n, at) ->
        ui.ListItem(item: n, presence: at)
    }
}

view note_board(nb: Notebook) {
    std.page(nb) {
        text("Board")
    }
}"#,
    );
    assert!(plain.meta.views_of.is_empty(), "no views are declared");
}

#[test]
fn a_views_declaration_names_the_types_own_views_and_its_default_one() {
    // A view of another type is no view of this one.
    expect(
        "E018",
        r"type Note {
    title: Text
}

root Notebook

type Notebook {
    notes: Collection<Note>
}

default view note_cards for Notebook

view note_cards(nb: Notebook) {
    std.list(nb.notes) { (n, at) ->
        ui.ListItem(item: n, presence: at)
    }
}

view note_page(n: Note) {
    std.document(n) {
        text(n.title)
    }
}

views note_cards, note_page for Notebook",
    );
    // The default view is the one a window opens on, so the switch names it.
    expect(
        "E018",
        r#"type Note {
    title: Text
}

root Notebook

type Notebook {
    notes: Collection<Note>
}

default view note_cards for Notebook

view note_cards(nb: Notebook) {
    std.list(nb.notes) { (n, at) ->
        ui.ListItem(item: n, presence: at)
    }
}

view note_board(nb: Notebook) {
    std.page(nb) {
        text("Board")
    }
}

views note_board for Notebook"#,
    );
    ok(r#"type Note {
    title: Text
}

root Notebook

type Notebook {
    notes: Collection<Note>
}

default view note_cards for Notebook

view note_cards(nb: Notebook) {
    std.list(nb.notes) { (n, at) ->
        ui.ListItem(item: n, presence: at)
    }
}

view note_board(nb: Notebook) {
    std.page(nb) {
        text("Board")
    }
}

views note_cards, note_board for Notebook"#);
}

// USER REQUIREMENT (2026-10-20): "A package declares scenarios:
// named seed data for screenshots, tests and benchmarks ... They never touch a person's data."
#[test]
fn a_scenario_is_a_hidden_action_listed_with_its_name() {
    let c = notes();
    let key = "scenario \"a week of notes\"";
    let id = c
        .ids
        .get(key)
        .expect("the scenario has an id in the lock")
        .to_string();
    assert_eq!(c.meta.scenarios.len(), 1);
    let s = &c.meta.scenarios[0];
    assert_eq!(
        (s.id.as_str(), s.name.as_str(), s.action.as_str()),
        (id.as_str(), "a week of notes", id.as_str())
    );
    let a = c
        .meta
        .actions
        .iter()
        .find(|a| a.id == id)
        .expect("its action");
    assert!(a.scenario && a.params.is_empty() && a.result.is_none());
    assert_eq!(a.class, tilolang::types::EffectClass::Undoable);
    assert_eq!(a.label, "a week of notes");
    assert!(
        c.meta.offered_actions().all(|x| x.id != id),
        "the platform never offers it"
    );
    assert!(c.ids.to_text().contains(key));
    crate::common::validate(&c.wasm);
}

#[test]
fn an_action_that_calls_another_says_so_in_its_metadata() {
    let c = compile_body(
        r#"type T {
    t: Text
}

action mark(t: T) {
    t.t = "done"
}

action twice(t: T) {
    mark(t)
    mark(t)
}

action once(t: T) {
    t.t = "one"
}"#,
    );
    let invokes = |name: &str| {
        c.meta
            .actions
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("the action {name}"))
            .invokes
    };
    assert!(invokes("twice"), "a body that calls an action is run again");
    assert!(!invokes("once"), "a body that calls none is one change");
    assert!(!invokes("mark"), "and so is what `twice` calls");
}

#[test]
fn metadata_without_the_new_fields_still_reads() {
    let c = compile_body(
        "type T {\n    t: Text\n}\nenum E {\n    a\n}\naction go(x: T) {\n    x.t = \"\"\n}",
    );
    let mut json: serde_json::Value =
        serde_json::from_slice(&c.meta.to_json_bytes()).expect("json");
    for a in json["actions"].as_array_mut().expect("actions") {
        let o = a.as_object_mut().expect("object");
        o.remove("label");
        o.remove("scenario");
    }
    json["enums"][0]["variants"][0]
        .as_object_mut()
        .expect("variant")
        .remove("label");
    let old: PackageMeta = serde_json::from_value(json).expect("old metadata parses");
    assert_eq!(old.actions[0].label, "");
    assert!(old.scenarios.is_empty() && old.row_views.is_empty() && old.icon.is_none());
}

// ---- errors -------------------------------------------------------------------------------

const T: &str =
    "type T {\n    t: Text\n    n: Int\n    b: Bool = false\n    items: Collection<T>\n}\n";

fn with_t(rest: &str) -> String {
    format!("{T}{rest}")
}

#[test]
fn e090_action_modifiers_and_options() {
    expect(
        "E090",
        &with_t("primary destructive action a(x: T) {\n    x.n = 1\n}"),
    );
    expect(
        "E090",
        &with_t(
            "primary action a(x: T) {\n    x.n = 1\n}\nprimary action b(x: T) {\n    x.n = 2\n}",
        ),
    );
    expect(
        "E090",
        &with_t("action a(x: T) (style: .plus) {\n    x.n = 1\n}"),
    );
    expect(
        "E090",
        &with_t("action a(x: T) (label: x.t) {\n    x.n = 1\n}"),
    );
    expect("E090", "primary action a() {\n    return\n}");
    ok(&with_t(
        "primary action a(x: T) {\n    x.n = 1\n}\ndestructive action b(x: T) {\n    x.n = 2\n}",
    ));
    assert_eq!(
        codes(&with_t("primary fn f() -> Int {\n    return 1\n}")),
        vec!["E003"],
        "modifiers mark actions only"
    );
}

#[test]
fn e091_unknown_icons() {
    expect(
        "E091",
        &with_t("action a(x: T) (icon: .not_an_icon) {\n    x.n = 1\n}"),
    );
    let err = tilolang::compile_package(
        "app t.x \"X\" (icon: .pinn)\n",
        &tilolang::IdsLock::new(),
        &[],
    )
    .expect_err("fails");
    assert_eq!(err[0].code, "E091");
    assert!(err[0].message.contains(".pin"), "{}", err[0].message);
    expect(
        "E091",
        &with_t("row view r for T\nview r(x: T) {\n    row(x.t, mark: .nope)\n}"),
    );
    let err = tilolang::compile_package(
        "app t.x \"X\"\nenum E {\n    a (icon: .pinn)\n}\n",
        &tilolang::IdsLock::new(),
        &[],
    )
    .expect_err("fails");
    assert_eq!(err[0].code, "E091");
    assert!(err[0].message.contains(".pin"), "{}", err[0].message);
    expect("E090", "enum E {\n    a (label: .pin)\n}");
    expect("E090", "enum E {\n    a (icon: \"pin\")\n}");
}

#[test]
fn a_field_is_its_name_its_type_and_its_default() {
    // What a record is shown by comes from its type's `row view` and the app's own views; a
    // field says nothing of it.
    expect("E003", "type T {\n    n: Int (title)\n}");
    expect("E003", "type T {\n    n: Int (done)\n}");
    expect("E003", "type T {\n    n: Option<Date> (time of: due)\n}");
    expect("E003", "type T {\n    n: Option<Bytes> (asked)\n}");
    ok("type T {\n    a: Text\n    b: Text merge register = \"x\"\n}");
}

#[test]
fn e093_row_views() {
    expect(
        "E093",
        &with_t("row view r for T\nview r(x: T, y: Int) {\n    row(x.t)\n}"),
    );
    expect(
        "E093",
        &with_t("row view r for T\nview r(x: T) {\n    text(x.t)\n}"),
    );
    expect(
        "E093",
        &with_t(
            "row view r for T\nrow view s for T\nview r(x: T) {\n    row(x.t)\n}\nview s(x: T) {\n    row(x.t)\n}",
        ),
    );
    ok(&with_t(
        "row view r for T\nview r(x: T) {\n    let last = match x.items.last() {\n        .some(i) => i.t,\n        .none => \"\",\n    }\n    row(last)\n}",
    ));
    expect(
        "E093",
        &with_t("row view r for T\nview r(x: T) {\n    row(x.t)\n    let y = 1\n}"),
    );
}

/// A row shows a value derived from its resource, or a resource itself; a card counts.
#[test]
fn rows_of_derived_values_and_resources() {
    ok(&with_t(
        "view v(x: T) {\n    list(x.items) { i ->\n        row(i, meta: i.t, avatar: invoker_of(i))\n    }\n}\nfn invoker_of(i: T) -> Principal {\n    i.owner\n}",
    )
    .replace("    items: Collection<T>\n", "    items: Collection<T>\n    owner: Principal\n"));
    ok(&with_t(
        "fn ends(x: T) -> Int {\n    let a: Option<T> = x.items.first()\n    let b: Option<T> = x.items.last()\n    let n: Option<Int> = [1, 2].last()\n    match n {\n        .some(v) => v,\n        .none => 0,\n    }\n}",
    ));
    expect("E027", &with_t("fn f(x: T) -> Int {\n    x.t.last()\n}"));
    ok(&with_t(
        "view v(x: T) {\n    ui.Card(item: x, title: meta(x).title, count: x.items.len()) {\n        text(x.t)\n    }\n}",
    ));
    expect(
        "E020",
        &with_t(
            "view v(x: T) {\n    ui.Card(item: x, title: meta(x).title, count: x.t) {\n        text(x.t)\n    }\n}",
        ),
    );
    expect(
        "E020",
        &with_t("view v(x: T) {\n    list(x.items) { i ->\n        row(i.b)\n    }\n}"),
    );
}

#[test]
fn e094_scenarios_and_root() {
    let base = with_t(
        "root T\ndefault view v for T\nview v(x: T) {\n    page {\n        text(x.t)\n    }\n}\n",
    );
    ok(&format!(
        "{base}scenario \"one\" {{\n    let r = root T\n    r.n = 1\n}}"
    ));
    expect(
        "E094",
        &format!(
            "{base}scenario \"one\" {{\n    let r = root T\n}}\nscenario \"one\" {{\n    let r = root T\n}}"
        ),
    );
    // A root is an expression anywhere: an action, a fn and a view reach it too.
    ok(&format!(
        "{base}action a(x: T) {{\n    let r = root T\n    r.n = 2\n}}"
    ));
    ok(&format!(
        "{base}fn f() -> Int {{\n    let r = root T\n    r.n\n}}"
    ));
    ok(&format!(
        "{base}view w() {{\n    let r = root T\n    text(r.t)\n}}"
    ));
    expect(
        "E094",
        &format!("{base}type U {{\n    n: Int\n}}\nscenario \"s\" {{\n    let r = root U\n}}"),
    );
    expect(
        "E094",
        &format!("{base}scenario \"a = b\" {{\n    let r = root T\n}}"),
    );
}

/// Only a scenario runs an action as a named person: elsewhere a principal comes from `invoker`.
#[test]
fn a_scenario_runs_actions_as_named_people() {
    let base = with_t(
        "root T\ndefault view v for T\nview v(x: T) {\n    page {\n        text(x.t)\n    }\n}\naction bump(x: T) {\n    x.n = 1\n}\n",
    );
    ok(&format!(
        "{base}scenario \"two\" {{\n    let r = root T\n    let mia = person(\"Mia\")\n    invoke(bump(r), by: mia)\n    invoke(bump(r), by: person(\"Leo\"))\n}}"
    ));
    expect(
        "E094",
        &format!("{base}action a(x: T) {{\n    let p = person(\"Mia\")\n}}"),
    );
    expect(
        "E094",
        &format!("{base}action a(x: T) {{\n    invoke(bump(x), by: invoker)\n}}"),
    );
    expect(
        "E020",
        &format!(
            "{base}scenario \"s\" {{\n    let r = root T\n    invoke(bump(r), by: \"Mia\")\n}}"
        ),
    );
    expect(
        "E020",
        &format!("{base}scenario \"s\" {{\n    let r = root T\n    let p = person(1)\n}}"),
    );
}

/// A scenario may return the resource the window shows once it is staged, as a primary action
/// returns what it made; it must then return one on every path.
#[test]
fn a_scenario_returns_what_the_window_shows() {
    let base = with_t(
        "root T\ndefault view v for T\nview v(x: T) {\n    page {\n        text(x.t)\n    }\n}\n",
    );
    ok(&format!(
        "{base}scenario \"open\" -> T {{\n    let r = root T\n    r.n = 1\n    r\n}}"
    ));
    expect(
        "E029",
        &format!("{base}scenario \"open\" -> T {{\n    let r = root T\n    r.n = 1\n}}"),
    );
    let src = format!(
        "app so.tilo.t \"T\"\n{base}scenario \"open\" -> T {{\n    let r = root T\n    r\n}}\n"
    );
    let compiled = crate::common::compile(&src);
    let scenario = compiled
        .meta
        .actions
        .iter()
        .find(|action| action.scenario)
        .expect("the scenario's action");
    assert!(scenario.result.is_some(), "{scenario:?}");
    let printed = tilolang::print_module(&tilolang::parse(&src).expect("parses"));
    assert!(printed.contains("scenario \"open\" -> T {"), "{printed}");
}

/// A board's scenario attaches the apps it shows: `root alias.T` reaches a root of a package it
/// uses. (What fills those roots is their own packages' scenarios; one change never spans apps.)
#[test]
fn a_scenario_reaches_the_roots_of_the_packages_it_uses() {
    let notes = notes().meta;
    let board = |line: &str| {
        format!(
            "app so.tilo.board \"Board\"\nuse so.tilo.notes as notes\ntype Board {{\n    nb: Option<notes.Notebook>\n}}\nroot Board\ndefault view board for Board\nview board(b: Board) {{\n    page {{\n        text(\"Board\")\n    }}\n}}\nscenario \"a board\" {{\n    let b = root Board\n    {line}\n}}\n"
        )
    };
    let attached = crate::common::compile_with(
        &board("let nb = root notes.Notebook\n    b.nb = .some(nb)"),
        std::slice::from_ref(&notes),
    );
    assert!(attached.is_ok(), "{:?}", attached.err());
    let not_a_root = crate::common::compile_with(
        &board("let n = root notes.Note"),
        std::slice::from_ref(&notes),
    )
    .expect_err("a Note is not a root");
    assert!(
        not_a_root
            .iter()
            .any(|diagnostic| diagnostic.code == "E094"),
        "{not_a_root:?}"
    );
}

// A window's view is what it builds: a frame of the standard library (`std.list`,
// `std.document`, `std.page`), the toolkit's components composed (`ui.NavShell`), or any view.
// The library's frames check their parts as any view's parameters are checked.
#[test]
fn a_window_is_what_its_view_builds_and_std_frames_check_their_parts() {
    let head = with_t("root T\ndefault view v for T\n");
    ok(&format!(
        "{head}view v(x: T) {{\n    stack {{\n        text(x.t)\n    }}\n}}"
    ));
    ok(&format!(
        "{head}view v(x: T) {{\n    state n: Int = 0\n    let s = x.t\n    std.page(x, canvas: true) {{\n        text(s)\n    }}\n}}"
    ));
    ok(&format!(
        "{head}view v(x: T) {{\n    std.list(x.items) {{ (i, at) ->\n        ui.ListItem(item: i, presence: at)\n    }}\n}}"
    ));
    ok(&format!(
        "{head}view v(x: T) {{\n    page(full) {{\n        text(x.t)\n    }}\n}}"
    ));
    expect(
        "E060",
        &with_t(
            "view v(x: T) {\n    stack {\n        sidebar {\n            text(x.t)\n        }\n    }\n}",
        ),
    );
    expect(
        "E066",
        &with_t("view v(x: T) {\n    std.places(x.t, filters(T), \"All\", icon: .none)\n}"),
    );
    expect("E097", &with_t("view v(x: T) {\n    std.list(x.items)\n}"));
}

#[test]
fn e096_removed_vocabulary_names_its_replacement() {
    for body in [
        "view v() {\n    heading(\"Notes\")\n}",
        "view v() {\n    list_row(\"a\", secondary: \"b\")\n}",
        "view v() {\n    text(\"a\", style: .caption)\n}",
        "view v() {\n    button(\"a\", style: .primary)\n}",
        "view v() {\n    button(\"a\", size: .small)\n}",
        "view v() {\n    column {\n        text(\"a\")\n    }\n}",
        "view v() {\n    card(1)\n}",
        "view v() {\n    date(\"x\")\n}",
        "view v() {\n    image(\"a.png\", alt: \"a\", width: 4rem)\n}",
    ] {
        assert_eq!(codes(body), vec!["E096"], "{body}");
    }
}

#[test]
fn e097_lists_and_rows() {
    expect(
        "E097",
        "view v(xs: List<Int>) {\n    list(xs) { x ->\n        text(\"{x}\")\n    }\n}",
    );
    expect("E097", &with_t("view v(x: T) {\n    list(x.items)\n}"));
    expect(
        "E097",
        &with_t("view v(x: T) {\n    list(x.items) {\n        text(x.t)\n    }\n}"),
    );
    expect(
        "E096",
        &with_t(
            "view v(x: T) {\n    state s: Int = 0\n    list(x.items, sort: s) { i ->\n        row(i.t)\n    }\n}",
        ),
    );
    expect(
        "E097",
        &with_t("view v(x: T) {\n    stack {\n        row(x.t)\n    }\n}"),
    );
    expect(
        "E097",
        &with_t(
            "view v(x: T) {\n    list(x.items) { i ->\n        row(i.t) {\n            text(i.t)\n        }\n    }\n}",
        ),
    );
    expect(
        "E097",
        &with_t("view v(x: T) {\n    list(x.items) { i ->\n        row(i.t, mark: i.n)\n    }\n}"),
    );
    ok(&with_t(
        "view v(x: T) {\n    list(x.items) { i ->\n        row(i.t, secondary: i.t, meta: \"m\", badge: \"b\", value: i.n, icon: .pin, checkbox: i.b, mark: if i.b { .pin } else { .check })\n    }\n}",
    ));
}

#[test]
fn e098_controls_and_facts_misused() {
    expect("E098", &with_t("view v(x: T) {\n    choice(bind: x.b)\n}"));
    expect(
        "E098",
        &with_t("view v(x: T) {\n    facts(x) {\n        \"Name\": x.t\n    }\n}"),
    );
    expect(
        "E098",
        &with_t("view v(x: T) {\n    stack {\n        \"Name\": x.t\n    }\n}"),
    );
    expect(
        "E098",
        &with_t(
            "action go(x: T) {\n    x.n = 1\n}\nview v(x: T) {\n    button(go(x), on: go(x))\n}",
        ),
    );
    ok(&with_t(
        "enum K {\n    a\n    b\n}\naction go(x: T) {\n    x.n = 1\n}\nview v(x: T) {\n    state k: K = .a\n    stack {\n        choice(bind: k)\n        facts(x)\n        toggle(bind: x.b)\n        button(go(x), enabled: x.b)\n        button(\"Pick b\", on: set(k, .b))\n    }\n}",
    ));
}

// A date field picks a Date, like a toggle a Bool: any control that binds a value checks it
// against what it binds.
#[test]
fn a_date_field_binds_a_date() {
    let head = "type Task {\n    due: Option<Date> = .none\n    on: Date = Date(\"2026-10-24\")\n    t: Text\n}\n";
    ok(&format!(
        "{head}view v(x: Task) {{\n    state d: Date = Date(\"2026-01-01\")\n    state o: Option<Date> = .none\n    stack {{\n        ui.DateField(bind: x.due, optional: true)\n        ui.DateField(bind: x.on)\n        ui.DateField(bind: d)\n        ui.DateField(bind: o, optional: true)\n    }}\n}}"
    ));
    expect(
        "E062",
        &format!("{head}view v(x: Task) {{\n    ui.DateField(bind: x.t)\n}}"),
    );
    expect(
        "E022",
        &format!("{head}view v(x: Task) {{\n    ui.DateField(\"x\")\n}}"),
    );
}

const DATED: &str = "type Task {\n    title: Text\n    due: Option<Date> = .none\n    start: Date = Date(\"2026-10-24\")\n    done_at: Option<Time> = .none\n}\n";

#[test]
fn dates_and_times_are_nominal_iso_text() {
    let c = compile_body(&format!(
        "{DATED}action finish(t: Task) uses time {{\n    t.done_at = now\n    if t.start < Date(\"2027-01-01\") and now >= Time(\"2026-10-21T09:30:00Z\") {{\n        t.title = \"{{t.start}} at {{now}}\"\n    }}\n    t.due = .some(Date(\"2028-02-29\"))\n}}"
    ));
    let task = &c.meta.types[0];
    let ty = |f: &str| {
        task.fields
            .iter()
            .find(|x| x.name == f)
            .expect("field")
            .ty
            .clone()
    };
    assert_eq!(ty("due"), "Option<Date>");
    assert_eq!(ty("start"), "Date");
    assert_eq!(ty("done_at"), "Option<Time>");
    let start = task
        .fields
        .iter()
        .find(|x| x.name == "start")
        .expect("start");
    assert_eq!(start.default, Some(serde_json::json!("2026-10-24")));
    assert_eq!(start.merge, tilolang::types::MergeKind::Register);
    let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has("now") && has("str_cmp"));
    crate::common::validate(&c.wasm);
}

#[test]
fn dates_and_times_are_checked() {
    let act = |body: &str| format!("{DATED}action a(t: Task) {{\n    {body}\n}}");
    for bad in [
        "t.start = Date(\"2026-13-01\")",
        "t.start = Date(\"2026-02-30\")",
        "t.start = Date(\"24.10.2026\")",
        "t.start = Date(t.title)",
        "t.done_at = .some(Time(\"2026-10-21 09:30\"))",
        "t.done_at = .some(Time(\"2026-10-21T24:00:00Z\"))",
    ] {
        assert_eq!(codes(&act(bad)), vec!["E099"], "{bad}");
    }
    expect("E020", &act("t.start = \"2026-10-24\""));
    expect("E020", &act("t.title = t.start"));
    expect("E020", &act("t.done_at = .some(Date(\"2026-10-24\"))"));
    expect("E020", &act("let x = t.start < now"));
    expect("E024", &act("let x = t.due < t.due"));
    ok(&act("t.title = \"due {t.start}\""));
    expect(
        "E049",
        &format!("{DATED}view v(t: Task) {{\n    text(\"{{now}}\")\n}}"),
    );
    expect("E049", &format!("{DATED}fn f() -> Time {{\n    now\n}}"));
    expect("E099", "type T {\n    d: Date = Date(\"x\")\n}");
    expect(
        "E020",
        "type T {\n    d: Date = Time(\"2026-10-21T09:30:00Z\")\n}",
    );
    expect("E011", "type Date {\n    d: Int\n}");
}

// A Date or Time a UI prop receives is its ISO text, and `<prop>:as` says which, for the host
// to format it; `.none` is the empty text.
#[test]
fn dated_props_say_what_they_are() {
    let c = compile_body(&format!(
        "{DATED}type L {{\n    tasks: Collection<Task>\n}}\nview v(l: L) {{\n    list(l.tasks) {{ t ->\n        row(t.title, meta: t.due, secondary: t.start)\n    }}\n}}\nview d(t: Task) {{\n    stack {{\n        text(t.start)\n        facts(t)\n    }}\n}}"
    ));
    let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has("meta:as") && has("secondary:as"));
    crate::common::validate(&c.wasm);
}

// One control per action. An action whose `on:` is its subject's
// field says which state it toggles, so a window that binds a control to that field shows the
// control alone.
#[test]
fn an_action_whose_on_is_its_subjects_field_says_which_state_it_toggles() {
    let src = "app so.tilo.todo \"Todo\"\n\
        type Todo {\n    title: Text\n    done: Bool = false\n    starred: Bool = false\n}\n\
        action check(item: Todo, done: Bool = not item.done) (icon: .check, on: item.done) {\n    item.done = done\n}\n\
        action flip(item: Todo, starred: Bool = not item.starred) (icon: .check, on: not item.starred) {\n    item.starred = starred\n}\n\
        action star(item: Todo) (icon: .plus) {\n    item.starred = true\n}\n";
    let m = crate::common::compile(src).meta;
    let todo = m.types.iter().find(|t| t.name == "Todo").expect("Todo");
    let done = todo.fields.iter().find(|f| f.name == "done").expect("done");
    assert_eq!(action(&m, "check").toggles.as_ref(), Some(&done.id));
    assert_eq!(
        action(&m, "flip").toggles,
        None,
        "an `on:` that is not the field itself names no field"
    );
    assert_eq!(action(&m, "star").toggles, None, "no `on:`, no state");
}

// An action declares the shortcut that runs it while its window has the keyboard, published as
// one spelling: the modifiers in one order and the key's token, which the toolkit matches a
// press against with the platform's own command modifier.
#[test]
fn an_action_carries_the_shortcut_it_declares() {
    let m = compile_body(
        r#"type Thread {
    subject: Text
}
action archive(t: Thread) (icon: .archive, key: "e") {}
action reply(t: Thread) (key: "Ctrl+Enter") {}
action share(t: Thread) (key: "control+shift+Return") {}
action nothing(t: Thread) {}"#,
    )
    .meta;
    assert_eq!(action(&m, "archive").key.as_deref(), Some("e"));
    assert_eq!(action(&m, "reply").key.as_deref(), Some("Ctrl+enter"));
    assert_eq!(
        action(&m, "share").key.as_deref(),
        Some("Ctrl+Shift+enter"),
        "one spelling, whatever order and case it was written in"
    );
    assert_eq!(action(&m, "nothing").key, None, "no `key:`, no shortcut");
    // A shortcut is modifiers and one key: no second key, no key that does not exist, and no
    // chord that stops before its key.
    for wrong in [r#"key: "ee""#, r#"key: "Entr""#, r#"key: "Ctrl+""#] {
        let said = format!(
            r"type Thread {{
    subject: Text
}}
action a(t: Thread) ({wrong}) {{}}"
        );
        expect("E090", &said);
    }
}

// Two actions of one subject cannot answer the same keys: one would silently take the other's.
// The same key on another subject is another place, and an action on the app's root answers
// window-wide against the other root actions, not against an item's.
#[test]
fn two_actions_of_one_subject_share_no_shortcut() {
    let thread = r"type Thread {
    subject: Text
}
type Draft {
    subject: Text
}";
    expect(
        "E090",
        &format!(
            r#"{thread}
action archive(t: Thread) (key: "Ctrl+E") {{}}
action reply(t: Thread) (key: "control+e") {{}}"#
        ),
    );
    ok(&format!(
        r#"{thread}
action archive(t: Thread) (key: "Ctrl+E") {{}}
action pin(d: Draft) (key: "Ctrl+E") {{}}"#
    ));
    expect(
        "E090",
        &format!(
            r#"{thread}
type Threads {{
    threads: Collection<Thread>
}}
root threads: Threads
default view thread_list for Threads
row view thread_row for Thread
view thread_row(t: Thread) {{
    row(t.subject)
}}
view thread_list(threads: Threads) {{
    std.list(threads.threads) {{ (t, at) ->
        ui.ListItem(item: t, presence: at) {{
            thread_row(t)
        }}
    }}
}}
primary action new_thread(threads: Threads) -> Thread (key: "Ctrl+N") {{ return threads.threads.insert(Thread(subject: "")) }}
action tidy(threads: Threads) (key: "Ctrl+N") {{ threads.threads.remove_where(s -> s.subject == "") }}"#
        ),
    );
    ok(&format!(
        r#"{thread}
type Threads {{
    threads: Collection<Thread>
}}
root threads: Threads
default view thread_list for Threads
row view thread_row for Thread
view thread_row(t: Thread) {{
    row(t.subject)
}}
view thread_list(threads: Threads) {{
    std.list(threads.threads) {{ (t, at) ->
        ui.ListItem(item: t, presence: at) {{
            thread_row(t)
        }}
    }}
}}
action new_thread(threads: Threads) -> Thread (key: "Ctrl+N") {{ return threads.threads.insert(Thread(subject: "")) }}
action archive(t: Thread) (key: "Ctrl+N") {{}}"#
    ));
}
