//! native-apps.md sections 1, 3, 5 and 7: what declarations tell the platform (labels, roles,
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
    title: Text (title)
    body: Text (body)
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
action toggle_pin(n: Note) (icon: .pin) {
    n.pinned = not n.pinned
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
    window filter: NoteFilter = .all
    list(nb.notes, filter: filter, empty: "No notes yet")
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

// USER REQUIREMENT (2026-10-20, native-apps.md section 1): "An action's label is its name in
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
    let role = |f: &str| {
        note.fields
            .iter()
            .find(|x| x.name == f)
            .and_then(|x| x.role.clone())
    };
    assert_eq!(role("title").as_deref(), Some("title"));
    assert_eq!(role("body").as_deref(), Some("body"));
    assert_eq!(role("pinned"), None);
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
        "/// Which notes show.\nenum NoteFilter {\n    /// All notes\n    all (icon: .file_text)\n    pinned (icon: .pin)\n    shared(by: Text) (icon: .users)\n    recent\n}",
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
    assert_eq!(
        e.variants[2].fields.len(),
        1,
        "a payload and an icon together"
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

const PIN: &str = "type Note {\n    title: Text (title)\n    pinned: Bool = false\n}\n/// Pin the note.\naction toggle_pin(n: Note) (label: if n.pinned { \"Unpin\" } else { \"Pin\" }, icon: if n.pinned { .bell_off } else { .pin }, on: n.pinned) {\n    n.pinned = not n.pinned\n}";

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
        printed.contains("(label: if n.pinned { \"Unpin\" } else { \"Pin\" }, icon: if n.pinned { .bell_off } else { .pin }, on: n.pinned)"),
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
fn row_views_are_in_the_metadata() {
    let c = notes();
    let id = |n: &str| c.ids.get(n).expect("id").to_string();
    assert_eq!(c.meta.row_view(&id("Note")), Some(id("note_row").as_str()));
    assert_eq!(c.meta.row_view(&id("Notebook")), None);
}

// USER REQUIREMENT (2026-10-20, native-apps.md section 5): "A package declares scenarios:
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
        codes(&with_t("primary flow f(x: T) {\n    return\n}")),
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
fn e092_field_markers() {
    expect("E092", "type T {\n    n: Int (title)\n}");
    expect(
        "E092",
        "type T {\n    a: Text (title)\n    b: Text (title)\n}",
    );
    expect("E092", "type T {\n    a: Text (heading)\n}");
    ok("type T {\n    a: Text (title)\n    b: Text merge register (body) = \"x\"\n}");
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
    expect(
        "E094",
        &format!("{base}action a(x: T) {{\n    let r = root T\n}}"),
    );
    expect(
        "E094",
        &format!("{base}type U {{\n    n: Int\n}}\nscenario \"s\" {{\n    let r = root U\n}}"),
    );
    expect(
        "E094",
        &format!("{base}scenario \"a = b\" {{\n    let r = root T\n}}"),
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

// USER REQUIREMENT (2026-10-20, native-apps.md section 3.1): "An app's root view returns one
// frame."
#[test]
fn e095_a_window_is_one_frame() {
    let head = with_t("root T\ndefault view v for T\n");
    expect(
        "E095",
        &format!("{head}view v(x: T) {{\n    stack {{\n        text(x.t)\n    }}\n}}"),
    );
    expect(
        "E095",
        &format!(
            "{head}view v(x: T) {{\n    page {{\n        text(x.t)\n    }}\n    text(x.t)\n}}"
        ),
    );
    ok(&format!(
        "{head}view v(x: T) {{\n    window n: Int = 0\n    let s = x.t\n    canvas {{\n        text(s)\n    }}\n}}"
    ));
    ok(&format!(
        "{head}view v(x: T) {{\n    list(x.items) {{ i ->\n        row(i.t)\n    }}\n}}"
    ));
    expect(
        "E095",
        &format!("{head}view v(x: T) {{\n    page(full) {{\n        text(x.t)\n    }}\n}}"),
    );
    expect(
        "E095",
        &with_t(
            "view v(x: T) {\n    stack {\n        sidebar {\n            text(x.t)\n        }\n    }\n}",
        ),
    );
    ok(&with_t(
        "view v(x: T) {\n    document {\n        sidebar {\n            text(x.t)\n        }\n        inspector(x) {\n            text(x.t)\n        }\n    }\n}",
    ));
    ok(&with_t(
        "view v(x: T) {\n    stack {\n        page {\n            text(x.t)\n        }\n    }\n}",
    ));
}

#[test]
fn e096_removed_vocabulary_names_its_replacement() {
    for body in [
        "view v() {\n    heading(\"Notes\")\n}",
        "view v() {\n    list_row(\"a\", secondary: \"b\")\n}",
        "view v() {\n    text(\"a\", style: .caption)\n}",
        "view v() {\n    button(\"a\", style: .primary)\n}",
        "view v() {\n    button(\"a\", size: .small)\n}",
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
        "E097",
        &with_t(
            "view v(x: T) {\n    window s: Int = 0\n    list(x.items, sort: s) { i ->\n        row(i.t)\n    }\n}",
        ),
    );
    expect(
        "E097",
        &with_t(
            "enum K {\n    a(n: Int)\n    b\n}\nview v(x: T) {\n    window s: K = .b\n    list(x.items, filter: s) { i ->\n        row(i.t)\n    }\n}",
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
        &with_t("view v(x: T) {\n    list(x.items) { i ->\n        row(i.t, mark: i.t)\n    }\n}"),
    );
    ok(&with_t(
        "view v(x: T) {\n    list(x.items, empty: \"Nothing\") { i ->\n        row(i.t, secondary: i.t, meta: \"m\", badge: \"b\", value: i.n, icon: .pin, checkbox: i.b, mark: if i.b { .pin } else { .check })\n    }\n}",
    ));
}

#[test]
fn e098_controls_and_facts_misused() {
    expect("E098", &with_t("view v(x: T) {\n    choice(bind: x.b)\n}"));
    expect(
        "E098",
        &with_t("view v(x: T) {\n    facts {\n        text(x.t)\n    }\n}"),
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
        "enum K {\n    a\n    b\n}\naction go(x: T) {\n    x.n = 1\n}\nview v(x: T) {\n    lens k: K = .a\n    stack {\n        choice(bind: k)\n        facts {\n            \"Name\": x.t; \"Count\": x.n\n        }\n        toggle(bind: x.b)\n        button(go(x), enabled: x.b)\n        button(\"Pick b\", on: set(k, .b))\n    }\n}",
    ));
}

#[test]
fn date_is_not_a_primitive() {
    let err = tilolang::compile_package(
        "app t.x \"X\"\nview v() {\n    date(\"x\")\n}\n",
        &tilolang::IdsLock::new(),
        &[],
    )
    .expect_err("fails");
    assert_eq!(err[0].code, "E060");
    let listed = err[0]
        .message
        .split("primitives:")
        .nth(1)
        .unwrap_or_default();
    assert!(!listed.contains("date"), "{}", err[0].message);
}
