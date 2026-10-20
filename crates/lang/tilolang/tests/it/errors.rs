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
    expect("E025", "fn f() -> Int {\n    let x = none\n    return 1\n}");
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
fn e044_do_outside_a_flow() {
    expect(
        "E044",
        &with_c("action a(c: C) {\n}\naction b(c: C) {\n    do a(c)\n}"),
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
fn e080_unsupported_sort() {
    expect(
        "E080",
        "fn f(l: List<Int>) -> List<Int> {\n    return l.sort_by(x -> x)\n}",
    );
}

#[test]
fn e081_foreign_fn_call() {
    let lib =
        compile("app so.tilo.lib \"Lib\"\nfn double(x: Int) -> Int {\n    return x * 2\n}\n").meta;
    let got = codes_with(
        "use so.tilo.lib as lib\nfn f() -> Int {\n    return lib.double(1)\n}",
        &[lib],
    );
    assert!(got.iter().any(|c| c == "E081"), "{got:?}");
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
