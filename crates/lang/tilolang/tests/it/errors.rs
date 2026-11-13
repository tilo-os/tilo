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
fn e003_flow_is_not_a_declaration() {
    let err = crate::common::compile_with("app t.x \"X\"\nflow f() {\n    return\n}\n", &[])
        .expect_err("fails");
    let d = &err[0];
    assert_eq!(d.code, "E003");
    assert!(
        d.message.contains("Write `action` in its place"),
        "{}",
        d.message
    );
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

/// A parameter no form can ask for says so where the action is run short, naming the action,
/// the parameter and its type, rather than reading as one the person forgot.
#[test]
fn e022_names_a_parameter_no_form_can_ask_for() {
    let body = "action remind(at: Time, headers: Bytes) {\n}\naction go() {\n    remind(now)\n}";
    let d = crate::common::compile_with(&format!("app t.x \"X\"\n{body}\n"), &[])
        .expect_err("a form cannot ask for a bytes value");
    let message = &d[0].message;
    assert_eq!(d[0].code, "E022");
    assert!(
        message.contains("remind") && message.contains("headers") && message.contains("Bytes"),
        "{message}"
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
fn e043_fn_calls_an_action() {
    expect(
        "E043",
        &with_c("action a(c: C) {\n}\nfn f(c: C) -> Int {\n    a(c)\n    return 1\n}"),
    );
}

#[test]
fn e044_try_outside_an_action() {
    expect(
        "E044",
        &with_c("action a(c: C) {\n}\nfn f(c: C) -> Int {\n    let r = try a(c)\n    return 1\n}"),
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
fn e020_an_if_without_else_is_the_option_of_its_branch() {
    // `let x = if b { 1 }` is an Option: where an Int is expected, an Option of one is not one.
    expect(
        "E020",
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
        "view v() {\n    state n: Int = 0\n    button(\"x\", on: set(m, 1))\n}",
    );
    // Only views have state.
    expect(
        "E066",
        "action a() {\n}\nfn f() -> Int {\n    return 1\n}\naction g() {\n    set(x, 1)\n}",
    );
    // State is declared at the top of the view body.
    expect(
        "E066",
        "view v() {\n    stack {\n        state n: Int = 0\n        text(\"x\")\n    }\n}",
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

/// A file is the person's own, asked for at their window: no action, fn or agent asks them.
#[test]
fn e067_a_file_is_chosen_from_a_view() {
    expect(
        "E067",
        &with_c("action a(c: C) {\n    file.save(f, name: \"c.md\")\n}"),
    );
    expect(
        "E067",
        &with_c("fn f(c: C) -> Action {\n    file.choose(a(c))\n}"),
    );
    // What `file` takes, and what it does not.
    expect(
        "E026",
        &with_c("view e(c: C) {\n    button(\"x\", on: file.copy(\"a\"))\n}"),
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
fn e047_builtin_in_the_wrong_form() {
    expect(
        "E047",
        "fn f() -> Text {\n    return model_call(\"k\", \"x\")\n}",
    );
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

const MIGRATED: &str = "type Item {\n    title: Text\n    shelf: Text\n}\n\ntype Shelf {\n    name: Text\n    items: Collection<Item>\n}\n\ntype Store {\n    shelves: Collection<Shelf>\n    items: Collection<Item>\n}\n";

#[test]
fn a_migration_is_never_offered() {
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
}

#[test]
fn every_action_is_offered_and_no_word_hides_one() {
    // An action is a thing somebody does: a person from a menu, an agent from its choices, a
    // program by name. None of them is kept out of the list to reserve it for a gesture.
    let compiled = crate::common::compile_body(
        "type Task {\n    title: Text\n    at: Int\n}\n\n\
         action place(t: Task, at: Int) {\n    t.at = at\n}\n\n\
         action rename(t: Task, title: Text) {\n    t.title = title\n}\n",
    );
    let offered: Vec<&str> = compiled
        .meta
        .offered_actions()
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(offered, ["place", "rename"], "both are offered as they are");
    // `internal` is no word an action is marked with: it names nothing at the start of an item.
    assert_eq!(
        crate::common::codes(
            "type Task {\n    title: Text\n    at: Int\n}\n\n\
             internal action place(t: Task, at: Int) {\n    t.at = at\n}\n"
        ),
        ["E003"]
    );
}

#[test]
fn two_commands_that_write_one_field_from_a_value_they_ask_for_are_one_command() {
    // A command that asks for a when and writes it, a second that asks for another and writes
    // the same field, and a third that writes it without asking: the pair are one command
    // under two names, and the preset is a command of its own.
    let when =
        "type Day {\n    day: Text\n}\ntype Task {\n    title: Text\n    due: Option<Day>\n}\n";
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
    let task = "type Task {\n    title: Text\n}\n";
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
fn a_parameters_label_is_written_after_its_type_and_only_where_a_person_gives_it() {
    let task = "type Task {\n    title: Text\n    done: Bool\n}\n";
    let labeled = |param: &str| {
        crate::common::codes(&format!(
            "{task}action finish(t: Task, {param}) {{\n    t.done = true\n}}\n"
        ))
    };
    assert_eq!(
        labeled("done: Bool (label: \"On the done list\") = true"),
        Vec::<String>::new()
    );
    assert_eq!(
        crate::common::codes(&format!(
            "{task}view show_task(t: Task, done: Bool (label: \"Done\")) {{\n    ui.Text(t.title)\n}}\n"
        )),
        ["E057"],
        "only an action's parameters are given to a person, so only they name one"
    );
    assert_eq!(
        crate::common::codes(&format!(
            "{task}enum Seen {{\n    once(at: Int (label: \"Seen when\"))\n}}\n"
        )),
        ["E057", "E110"],
        "a variant's field holds a value nothing gives, so it names none"
    );
    assert_eq!(
        labeled("above: Option<Task> (above, label: \"Above\")"),
        ["E003"],
        "a parameter says one thing of itself, a place or a label"
    );
    assert_eq!(
        labeled("done: Bool (name: \"On the done list\")"),
        ["E003"],
        "`label` is the option a parameter takes"
    );
    assert_eq!(
        labeled("done: Bool (label: \"{t.title}\") = true"),
        ["E003"],
        "a label is words as they read, not an interpolation"
    );
    assert_eq!(
        labeled("done: Bool (label: \"  \") = true"),
        ["E057"],
        "a label says what the parameter holds"
    );
    let misplaced = crate::common::compile_with(
        &format!("app t.x \"X\"\n{task}action finish(t: Task, done: Bool = true (label: \"On the done list\")) {{\n    t.done = done\n}}\n"),
        &[],
    )
    .err()
    .and_then(|d| d.into_iter().find(|d| d.code == "E003"))
    .map_or_else(
        || panic!("a label after a default says where it goes"),
        |d| d.message,
    );
    assert!(
        misplaced.contains("right after the parameter's type"),
        "the error says where a label is written: {misplaced}"
    );
}

#[test]
fn an_entry_fills_the_actions_one_text_parameter() {
    let task = "type Task {\n    title: Text\n}\n\
action add(t: Task, title: Text = \"\") {\n    t.title = title\n}\n\
action twice(t: Task, one: Text = \"\", two: Text = \"\") {\n    t.title = one + two\n}\n\
action close(t: Task) {\n    t.title = \"x\"\n}\n\
view places<T>(t: T, entry: Option<Action> = .none) {\n    ui.Text(\"x\")\n}\n";
    let entry = |made: &str| {
        crate::common::codes(&format!(
            "{task}view home(t: Task) {{\n    places(t, entry: .some({made}))\n}}\n"
        ))
    };
    assert_eq!(
        entry("add(t)"),
        Vec::<String>::new(),
        "the action's one `Text` parameter is what the words nothing else takes fill"
    );
    assert_eq!(
        entry("close(t)"),
        ["E068"],
        "an action with no `Text` parameter has nothing for an entry to fill"
    );
    assert_eq!(
        entry("twice(t)"),
        ["E068"],
        "an action with two `Text` parameters leaves the words no one of them"
    );
    let marked = crate::common::compile_with(
        &format!("app t.x \"X\"\n{task}action note(t: Task, title: Text (entry) = \"\") {{\n    t.title = title\n}}\n"),
        &[],
    )
    .err()
    .and_then(|d| d.into_iter().find(|d| d.code == "E003"))
    .map_or_else(
        || panic!("the marker says nothing"),
        |d| d.message,
    );
    assert!(
        marked.contains("one `Text` parameter"),
        "the error says why the marker is gone: {marked}"
    );
}

#[test]
fn e094_invoke_runs_only_in_a_scenario() {
    expect(
        "E094",
        "type C {\n    t: Text\n}\naction a(c: C) {\n    c.t = \"x\"\n}\nfn f(c: C) -> Int {\n    invoke(a(c), by: invoker)\n    1\n}\n",
    );
}

#[test]
fn e020_invoke_from_takes_a_resource() {
    expect(
        "E020",
        "type C {\n    t: Text\n}\nroot C\naction a(c: C) {\n    c.t = \"x\"\n}\nscenario \"a thing\" -> C {\n    invoke(a(root C), from: 1)\n}\n",
    );
}
