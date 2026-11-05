//! One test per error code, plus the shape of diagnostics.

use crate::common::{codes, codes_with, compile, todo_meta};
use tilolang::IdsLock;

const C: &str = "type C {\n    n: Int merge counter\n    r: Int\n    t: Text\n    o: Option<Int>\n    items: Collection<C>\n    s: Set<Text>\n}\n";

fn with_c(rest: &str) -> String {
    format!("{C}{rest}")
}

fn expect(code: &str, body: &str) {
    let got = codes(body);
    assert!(
        got.iter().any(|c| c == code),
        "expected {code} for:\n{body}\ngot {got:?}"
    );
}

fn expect_with(code: &str, body: &str) {
    let got = codes_with(body, &[todo_meta()]);
    assert!(
        got.iter().any(|c| c == code),
        "expected {code} for:\n{body}\ngot {got:?}"
    );
}

#[test]
fn e001_unexpected_character() {
    expect("E001", "fn f() -> Int {\n    return 1 $ 2\n}");
}

#[test]
fn e002_unterminated_string() {
    expect("E002", "fn f() -> Text {\n    return \"abc\n}");
}

#[test]
fn e003_syntax_error() {
    expect("E003", "type {");
}

#[test]
fn e004_number_out_of_range() {
    expect(
        "E004",
        "fn f() -> Int {\n    return 99999999999999999999\n}",
    );
}

#[test]
fn e005_unknown_escape() {
    expect("E005", "fn f() -> Text {\n    return \"a\\q\"\n}");
}

#[test]
fn e010_missing_app_header() {
    let err = tilolang::compile_package("fn f() -> Int {\n    return 1\n}\n", &IdsLock::new(), &[])
        .expect_err("fails");
    assert_eq!(err.first().map(|d| d.code.as_str()), Some("E010"));
}

#[test]
fn e011_duplicate_declaration() {
    expect(
        "E011",
        "fn f() -> Int {\n    return 1\n}\nfn f() -> Int {\n    return 2\n}",
    );
}

#[test]
fn e012_duplicate_parameter() {
    expect("E012", "fn f(a: Int, a: Int) -> Int {\n    return a\n}");
}

#[test]
fn e013_unknown_type() {
    expect("E013", "fn f(a: Nope) -> Int {\n    return 1\n}");
}

#[test]
fn e014_unknown_name() {
    expect("E014", "fn f() -> Int {\n    return zzz\n}");
}

#[test]
fn e015_unknown_package() {
    expect("E015", "use so.nothing.here as n");
}

#[test]
fn e016_name_case() {
    expect("E016", "type lower {\n}");
}

#[test]
fn e019_default_must_be_literal() {
    expect("E019", "type T {\n    a: Int = 1 + 2\n}");
}

#[test]
fn e020_type_mismatch() {
    expect("E020", "fn f() -> Int {\n    return \"x\"\n}");
}

#[test]
fn e021_unknown_field() {
    expect("E021", &with_c("fn f(c: C) -> Int {\n    return c.nope\n}"));
}

#[test]
fn e022_wrong_arguments() {
    expect(
        "E022",
        "fn g(a: Int) -> Int {\n    return a\n}\nfn f() -> Int {\n    return g()\n}",
    );
}

#[test]
fn e023_container_outside_field() {
    expect("E023", "fn f(a: Set<Text>) -> Int {\n    return 1\n}");
}

#[test]
fn e024_operator_operands() {
    expect("E024", "fn f() -> Int {\n    return 1 + 2.0\n}");
}

#[test]
fn e025_cannot_infer() {
    expect("E025", "fn f() -> Int {\n    let x = []\n    return 1\n}");
}

#[test]
fn e026_not_a_value() {
    expect(
        "E026",
        "fn g() -> Int {\n    return 1\n}\nfn f() -> Int {\n    return g\n}",
    );
}

#[test]
fn e027_unknown_method() {
    expect(
        "E027",
        "fn f() -> Int {\n    let a = 1\n    return a.nope()\n}",
    );
}

#[test]
fn e029_missing_return() {
    expect("E029", "fn f() -> Int {\n}");
}

// USER REQUIREMENT (2026-10-20): "The compiler rejects mutations that would lose intent, for
// example `+=` on a `register`."
#[test]
fn e030_plus_equals_on_a_register() {
    let body = with_c("action a(c: C) {\n    c.r += 1\n}");
    expect("E030", &body);
    let msg = tilolang::compile_package(&format!("app t.x \"X\"\n{body}\n"), &IdsLock::new(), &[])
        .expect_err("fails");
    assert!(
        msg.iter().any(|d| d.message.contains("merge counter")),
        "{msg:?}"
    );
}

#[test]
fn e031_assign_to_a_counter() {
    expect("E031", &with_c("action a(c: C) {\n    c.n = 1\n}"));
}

#[test]
fn e032_invalid_merge_kind() {
    expect("E032", "type T {\n    b: Bool merge counter\n}");
}

#[test]
fn e033_assign_to_a_container() {
    expect("E033", &with_c("action a(c: C) {\n    c.items = 1\n}"));
}

#[test]
fn e034_arithmetic_assignment_on_text() {
    expect("E034", &with_c("action a(c: C) {\n    c.t += \"x\"\n}"));
}

#[test]
fn e040_mutation_outside_an_action() {
    expect(
        "E040",
        &with_c("fn f(c: C) -> Int {\n    c.r = 1\n    return 1\n}"),
    );
}

#[test]
fn e041_action_calls_a_foreign_action() {
    expect_with(
        "E041",
        "use so.tilo.todo as todo\naction a(l: todo.TodoList, t: todo.Todo) {\n    todo.remove(l, t)\n}",
    );
}

#[test]
fn e043_fn_calls_an_action() {
    expect(
        "E043",
        &with_c("action a(c: C) {\n}\nfn f(c: C) -> Int {\n    a(c)\n    return 1\n}"),
    );
}

#[test]
fn e044_try_outside_a_flow() {
    expect(
        "E044",
        &with_c("action a(c: C) {\n}\naction b(c: C) {\n    let r = try a(c)\n}"),
    );
}

#[test]
fn e018_root_and_default_view() {
    // A root needs a default view: it is what opening it shows.
    expect("E018", "type T {\n    n: Int\n}\nroot T");
    // A default view shows the resource and nothing else.
    expect(
        "E018",
        "type T {\n    n: Int\n}\nview v(t: T, n: Int) {\n    text(n)\n}\ndefault view v for T",
    );
    // A root is a record type of the package.
    expect("E018", "enum E {\n    a\n}\nroot E");
}

const E: &str = "enum Shape {\n    dot\n    line(len: Int)\n    box(w: Int, h: Int)\n}\n";

// USER REQUIREMENT (2026-10-20): "Exhaustive `match e { .a => expr_or_block, .b(x, y) => ...,
// }` as an expression (error with the missing variants listed)."
#[test]
fn e035_match_is_exhaustive_and_names_the_missing_variants() {
    let body =
        format!("{E}fn f(s: Shape) -> Int {{\n    match s {{\n        .dot => 0,\n    }}\n}}");
    expect("E035", &body);
    let msg = tilolang::compile_package(&format!("app t.x \"X\"\n{body}\n"), &IdsLock::new(), &[])
        .expect_err("fails");
    assert!(
        msg.iter()
            .any(|d| d.code == "E035" && d.message.contains(".line") && d.message.contains(".box")),
        "{msg:?}"
    );
}

#[test]
fn e036_unknown_variant_or_payload() {
    expect("E036", &format!("{E}fn f() -> Shape {{\n    .circle\n}}"));
    expect(
        "E036",
        &format!(
            "{E}fn f(s: Shape) -> Int {{\n    match s {{\n        .dot => 0,\n        .line(a, b) => a,\n        .box(w, h) => w,\n    }}\n}}"
        ),
    );
}

#[test]
fn e037_a_variant_needs_a_known_type() {
    expect(
        "E037",
        &format!("{E}fn f() -> Int {{\n    let s = .dot\n    return 1\n}}"),
    );
}

#[test]
fn e038_an_if_value_needs_an_else() {
    expect(
        "E038",
        "fn f(b: Bool) -> Int {\n    let x = if b { 1 }\n    return x\n}",
    );
}

// USER REQUIREMENT (2026-10-20): layout and UI options are typed enums; "bare words are no
// longer accepted there (error suggesting the dot form)".
#[test]
fn e065_bare_option_words_suggest_the_dot_form() {
    let body = "view v() {\n    stack(gap: section) {\n        spacer\n    }\n}";
    expect("E065", body);
    let msg = tilolang::compile_package(&format!("app t.x \"X\"\n{body}\n"), &IdsLock::new(), &[])
        .expect_err("fails");
    assert!(
        msg.iter().any(|d| d.message.contains(".section")),
        "{msg:?}"
    );
}

#[test]
fn e066_view_state_misuse() {
    // `set` names view state the view declares.
    expect(
        "E066",
        "view v() {\n    window n: Int = 0\n    button(\"x\", on: set(m, 1))\n}",
    );
    // Only views have state.
    expect(
        "E066",
        "action a() {\n}\nfn f() -> Int {\n    return 1\n}\nflow g() {\n    set(x, 1)\n}",
    );
    // State is declared at the top of the view body.
    expect(
        "E066",
        "view v() {\n    stack {\n        lens n: Int = 0\n        text(\"x\")\n    }\n}",
    );
}

#[test]
fn e067_open_takes_a_resource_from_a_view() {
    expect("E067", "view v() {\n    button(\"x\", on: open(1))\n}");
    expect(
        "E067",
        &with_c(
            "view w(c: C, n: Int) {\n    text(n)\n}\nview v(c: C) {\n    button(\"x\", on: open(c, view: w))\n}",
        ),
    );
}

#[test]
fn e045_mutating_a_foreign_type() {
    expect_with(
        "E045",
        "use so.tilo.todo as todo\naction a(l: todo.TodoList) {\n    l.name = \"x\"\n}",
    );
}

#[test]
fn e046_calling_a_flow() {
    expect("E046", "flow one() {\n}\nflow two() {\n    one()\n}");
}

#[test]
fn e047_builtin_in_the_wrong_form() {
    expect("E047", "action a() {\n    model_call(\"k\", \"x\")\n}");
    expect(
        "E047",
        "type C {\n    items: Collection<C>\n}\n/// Adds one.\nprimary action add(c: C) (icon: .plus) {\n    c.items.insert(C())\n}\nview list(c: C) {\n    let offered = actions(c)\n    text(\"{offered.len()}\")\n}",
    );
}

#[test]
fn e050_lambda_outside_a_builtin() {
    expect(
        "E050",
        "fn f() -> Int {\n    let g = x -> x\n    return 1\n}",
    );
}

#[test]
fn e051_lambda_expected() {
    expect(
        "E051",
        "fn f(l: List<Int>) -> Int {\n    return l.count(1)\n}",
    );
}

#[test]
fn e060_unknown_ui_name() {
    expect("E060", "view v() {\n    nothing_here\n}");
}

#[test]
fn e061_not_a_view() {
    expect(
        "E061",
        "fn g() -> Int {\n    return 1\n}\nview v() {\n    g()\n}",
    );
}

#[test]
fn e062_bind_target() {
    expect(
        "E062",
        &with_c("view v(c: C) {\n    input(bind: c.items)\n}"),
    );
}

#[test]
fn e063_unknown_property() {
    expect("E063", "view v() {\n    text(\"x\", nope: 2)\n}");
}

#[test]
fn e070_pinned_references() {
    expect(
        "E070",
        &with_c("fn f() -> C {\n    return @t.x.C:abc@v1\n}"),
    );
}

#[test]
fn e071_unknown_reference_type() {
    expect(
        "E071",
        "fn f() -> Int {\n    let a = @nope.Zed:1\n    return 1\n}",
    );
}

#[test]
fn e080_a_map_is_not_read_as_a_whole() {
    expect(
        "E080",
        "type C {\n    m: Map<Int>\n}\nfn f(c: C) -> Int {\n    let x = c.m\n    return 1\n}",
    );
}

#[test]
fn another_packages_fn_is_checked_against_its_own_signature() {
    let lib =
        compile("app so.tilo.lib \"Lib\"\nfn double(x: Int) -> Int {\n    return x * 2\n}\n").meta;
    // Its arguments and its result, as the package declares them.
    assert_eq!(
        codes_with(
            "use so.tilo.lib as lib\nfn f() -> Int {\n    return lib.double(1) + 1\n}",
            std::slice::from_ref(&lib),
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        codes_with(
            "use so.tilo.lib as lib\nfn f() -> Int {\n    return lib.double(\"x\")\n}",
            std::slice::from_ref(&lib),
        ),
        vec!["E020"]
    );
    assert_eq!(
        codes_with(
            "use so.tilo.lib as lib\nfn f() -> Int {\n    return lib.double(1, 2)\n}",
            std::slice::from_ref(&lib),
        ),
        vec!["E022"]
    );
    assert_eq!(
        codes_with(
            "use so.tilo.lib as lib\nfn f() -> Int {\n    return lib.doubled(1)\n}",
            std::slice::from_ref(&lib),
        ),
        vec!["E014"]
    );
    assert_eq!(
        codes_with(
            "use so.tilo.lib as lib\nfn f() -> Int {\n    return lib.double\n}",
            &[lib],
        ),
        vec!["E026"],
        "a fn is called, not a value"
    );
}

#[test]
fn all_errors_are_reported_not_only_the_first() {
    let got = codes("fn f() -> Int {\n    return zzz\n}\nfn g() -> Int {\n    return \"x\"\n}");
    assert!(
        got.contains(&"E014".to_string()) && got.contains(&"E020".to_string()),
        "{got:?}"
    );
}

#[test]
fn diagnostics_carry_positions_and_json() {
    let err = tilolang::compile_package(
        "app t.x \"X\"\nfn f() -> Int {\n    return zzz\n}\n",
        &IdsLock::new(),
        &[],
    )
    .expect_err("fails");
    let d = err.first().expect("one");
    assert_eq!((d.line, d.col), (3, 12));
    assert!(d.render("app.tl").starts_with("app.tl:3:12: E014 "));
    let json = tilolang::diagnostics_json(&err);
    assert!(json.contains("\"code\": \"E014\""));
}

const MIGRATED: &str = "type Item {\n    title: Text (title)\n    shelf: Text (previously)\n}\n\ntype Shelf {\n    name: Text (title)\n    items: Collection<Item>\n}\n\ntype Store {\n    shelves: Collection<Shelf>\n    items: Collection<Item> (previously)\n}\n";

#[test]
fn only_a_migration_reads_what_a_type_held_previously() {
    let migration = "/// Puts each item on the first shelf.\nmigration action shelve(s: Store) {\n    if let first = s.shelves.first() {\n        for i in s.items {\n            first.items.move(i, to: first.items.len())\n        }\n    }\n}\n";
    let compiled = crate::common::compile_body(&format!("{MIGRATED}\n{migration}"));
    let shelve = compiled
        .meta
        .actions
        .iter()
        .find(|a| a.name == "shelve")
        .unwrap();
    assert!(shelve.is_migration());
    assert!(
        compiled.meta.offered_actions().all(|a| a.name != "shelve"),
        "a migration is never offered"
    );
    assert_eq!(compiled.meta.migrations().count(), 1);
    let elsewhere = "action count(s: Store) -> Int {\n    s.items.len()\n}\n";
    assert_eq!(
        crate::common::codes(&format!("{MIGRATED}\n{elsewhere}")),
        ["E114"]
    );
    let in_a_fn = "fn shelf_of(i: Item) -> Text {\n    i.shelf\n}\n";
    assert_eq!(
        crate::common::codes(&format!("{MIGRATED}\n{in_a_fn}")),
        ["E114"]
    );
}

#[test]
fn an_internal_action_is_an_action_no_surface_offers() {
    let compiled = crate::common::compile_body(
        "type Task {\n    title: Text (title)\n    at: Int\n}\n\n\
         internal action place(t: Task, at: Int) {\n    t.at = at\n}\n\n\
         action rename(t: Task, title: Text) {\n    t.title = title\n}\n",
    );
    let place = compiled
        .meta
        .actions
        .iter()
        .find(|a| a.name == "place")
        .unwrap();
    assert!(place.is_internal());
    assert!(
        compiled.meta.offered_actions().all(|a| a.name != "place"),
        "an internal action runs from the app's gestures, not from a list of commands"
    );
    assert!(
        compiled.meta.offered_actions().any(|a| a.name == "rename"),
        "the actions beside it are offered"
    );
}

#[test]
fn nothing_writes_what_a_type_held_previously() {
    let assigned = "action reshelve(i: Item) {\n    i.shelf = \"top\"\n}\n";
    assert_eq!(
        crate::common::codes(&format!("{MIGRATED}\n{assigned}")),
        ["E115"]
    );
    let created =
        "action stock(s: Shelf) {\n    s.items.insert(Item(title: \"x\", shelf: \"top\"))\n}\n";
    assert_eq!(
        crate::common::codes(&format!("{MIGRATED}\n{created}")),
        ["E115"]
    );
}

const NESTED: &str = "type Item {\n    title: Text (title)\n    done: Bool\n    parent: Option<Item> (parent) = .none\n    /// Whether it and the items under it are done.\n    derive folded: Bool = done and children.all(folded)\n    derive subtasks: Int = children.count\n    derive subtasks_done: Int = children.count(done)\n}\n";

#[test]
fn nothing_writes_a_derived_key() {
    // A derived key is the collection's, worked out by the store's index: an action writes the
    // facts it is worked out from, never the value.
    let assigned = "action check(i: Item) {\n    i.folded = true\n}\n";
    assert_eq!(
        crate::common::codes(&format!("{NESTED}{assigned}")),
        ["E119"]
    );
    let made = "action add(o: Shelf, title: Text) {\n    o.items.insert(Item(title: title, done: false, folded: true))\n}\n";
    assert_eq!(
        crate::common::codes(&format!(
            "{NESTED}type Shelf {{\n    items: Collection<Item>\n}}\n{made}"
        )),
        ["E119"]
    );
}

#[test]
fn a_derived_key_reads_the_keys_declared_before_it() {
    // Folding is worked out from finishing: a key reads itself and the keys over it, so what it
    // reads is worked out whatever order the collection's index walks the items in.
    let forward = "type Item {\n    title: Text (title)\n    done: Bool\n    parent: Option<Item> (parent) = .none\n    derive folded: Bool = finished and parent.all(folded)\n    derive finished: Bool = done\n}\n";
    assert_eq!(crate::common::codes(forward), ["E119"]);
    let over = "type Item {\n    title: Text (title)\n    done: Bool\n    parent: Option<Item> (parent) = .none\n    derive finished: Bool = done\n    derive folded: Bool = finished and parent.all(folded)\n}\n";
    assert_eq!(crate::common::codes(over), Vec::<String>::new());
}

#[test]
fn a_derived_key_reads_the_items_under_an_item_or_above_it_not_both() {
    // Each direction is walked its own way; one key cannot be worked out from both.
    let both = "type Item {\n    title: Text (title)\n    done: Bool\n    parent: Option<Item> (parent) = .none\n    derive busy: Bool = children.all(done) and parent.all(done)\n}\n";
    let errors = crate::common::compile_with(&format!("app t.x \"X\"\n{both}\n"), &[])
        .err()
        .unwrap_or_else(|| panic!("a key reading both ways is refused"));
    let said = errors
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(said.contains("a key reads one way"), "{said}");
}

#[test]
fn a_comparison_is_of_two_counts() {
    let flags = "type Item {\n    title: Text (title)\n    done: Bool\n    parent: Option<Item> (parent) = .none\n    derive has_steps: Bool = children.count > done\n}\n";
    assert_eq!(crate::common::codes(flags), ["E119"]);
    let counts = "type Item {\n    title: Text (title)\n    done: Bool\n    parent: Option<Item> (parent) = .none\n    derive has_steps: Bool = children.count > 0\n    derive all_done: Bool = children.count(done) == children.count\n}\n";
    assert_eq!(crate::common::codes(counts), Vec::<String>::new());
}

#[test]
fn a_derived_key_that_reads_the_items_under_it_says_which_field_they_stand_under() {
    let no_parent = "type Note {\n    title: Text\n    done: Bool\n    derive folded: Bool = done and children.all(done)\n}\n";
    let errors = crate::common::compile_with(&format!("app t.x \"X\"\n{no_parent}\n"), &[])
        .err()
        .unwrap_or_else(|| panic!("a type without a parent field cannot read the items under it"));
    let said = errors
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        said.contains("(parent)"),
        "the error says which field the items stand under: {said}"
    );
}

#[test]
fn two_commands_that_write_one_field_from_a_value_they_ask_for_are_one_command() {
    // A command that asks for a when and writes it, a second that asks for another and writes
    // the same field, and a third that writes it without asking: the pair are one command
    // under two names, and the preset is a command of its own.
    let when = "type Day {\n    day: Text\n}\ntype Task {\n    title: Text (title)\n    due: Option<Day>\n}\n";
    let schedule = "action schedule(t: Task, due: Option<Day>) {\n    t.due = due\n}\n";
    let preset = "action due_none(t: Task) {\n    t.due = .none\n}\n";
    assert_eq!(
        crate::common::codes(&format!("{when}{schedule}{preset}")),
        Vec::<String>::new()
    );
    let snooze = "action snooze(t: Task, until: Option<Day>) {\n    t.due = until\n}\n";
    assert_eq!(
        crate::common::codes(&format!("{when}{schedule}{snooze}")),
        ["E120"]
    );
}

#[test]
fn a_place_a_view_supplies_is_an_option_after_the_subject_and_named_above() {
    let task = "type Task {\n    title: Text (title)\n}\n";
    let places = |param: &str| {
        crate::common::codes(&format!(
            "{task}action move_under(t: Task, {param}) {{\n    t.title = \"x\"\n}}\n"
        ))
    };
    assert_eq!(places("above: Option<Task> (above)"), Vec::<String>::new());
    assert_eq!(places("above: Option<Task> (below)"), ["E054"]);
    assert_eq!(places("above: Task (above)"), ["E055"]);
    assert_eq!(
        crate::common::codes(&format!(
            "{task}action move_under(t: Task, x: Text, above: Option<Task> (above)) {{\n    x = \"x\"\n}}\n"
        )),
        ["E053"],
        "a place follows the subject it is offered on"
    );
    assert_eq!(
        crate::common::codes(&format!(
            "{task}view show_task(t: Task (above)) {{\n    ui.Text(t.title)\n}}\n"
        )),
        ["E052"],
        "only an action is offered on an item, so only its parameters have places"
    );
    assert_eq!(
        places("above: Option<Task> (above) = .none"),
        ["E056"],
        "a place is written after the type and takes no default"
    );
    let misplaced = crate::common::compile_with(
        &format!("app t.x \"X\"\n{task}action move_under(t: Task, above: Option<Task> = .none (above)) {{\n    t.title = \"x\"\n}}\n"),
        &[],
    )
    .err()
    .and_then(|d| d.into_iter().find(|d| d.code == "E003"))
    .map_or_else(
        || panic!("a place after a default says where it goes"),
        |d| d.message,
    );
    assert!(
        misplaced.contains("right after the parameter's type"),
        "the error says where a place is written: {misplaced}"
    );
}

#[test]
fn e094_made_from_runs_only_in_a_scenario() {
    expect(
        "E094",
        "type C {\n    t: Text\n}\nfn f(c: C) -> Int {\n    made from c { 1 }\n}\n",
    );
}

#[test]
fn e020_made_from_takes_a_resource() {
    expect(
        "E020",
        "type C {\n    t: Text\n}\nroot C\nscenario \"a thing\" -> C {\n    made from 1 { root C }\n}\n",
    );
}
