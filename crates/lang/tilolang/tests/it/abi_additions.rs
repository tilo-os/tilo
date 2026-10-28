//! The ABI additions for sorting, reading maps, boxed set elements and `try`.

use crate::common::{codes, compile_body as compile, validate};

#[test]
fn sort_by_compiles_to_a_parallel_key_list() {
    let c = compile(
        "type Item {\n    title: Text\n}\ntype L {\n    items: Collection<Item>\n}\nfn by_title(l: L) -> List<Item> {\n    return l.items.sort_by(i -> i.title)\n}\nfn ints(x: List<Int>) -> List<Int> {\n    return x.sort_by(n -> n)\n}\nfn bools(x: List<Int>) -> List<Int> {\n    return x.sort_by(n -> n > 2)\n}",
    );
    validate(&c.wasm);
}

#[test]
fn sort_by_needs_a_scalar_or_text_key() {
    let got =
        codes("fn f(x: List<List<Int>>) -> List<List<Int>> {\n    return x.sort_by(n -> n)\n}");
    assert_eq!(got, vec!["E020"]);
    assert_eq!(
        codes("fn f(x: List<Int>) -> List<Int> {\n    return x.sort_by(1)\n}"),
        vec!["E051"]
    );
}

#[test]
fn a_map_field_is_read_with_get_and_keys() {
    let c = compile(
        "type M {\n    m: Map<Int>\n}\nfn one(c: M) -> Int {\n    match c.m.get(\"a\") {\n        .some(n) => n,\n        .none => 0,\n    }\n}\nfn names(c: M) -> List<Text> {\n    return c.m.keys()\n}\nview v(c: M) {\n    for k in c.m.keys() {\n        text(k)\n    }\n}",
    );
    validate(&c.wasm);
}

#[test]
fn sets_and_maps_of_scalars_are_written_with_boxed_values() {
    let c = compile(
        "type S {\n    nums: Set<Int>\n    scores: Map<Float>\n}\naction a(s: S) {\n    s.nums.add(3)\n    s.nums.remove(2)\n    s.scores.put(\"x\", 1.5)\n}",
    );
    validate(&c.wasm);
    let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(
        has("some_i") && has("some_f"),
        "scalars are boxed for set and map writes"
    );
}

#[test]
fn try_reports_whether_an_invocation_worked() {
    let src = "type L {\n    n: Int merge counter\n}\naction bump(l: L) {\n    l.n += 1\n}\naction size(l: L) -> Int {\n    return l.n\n}\nflow f(l: L) -> Int {\n    let ok = try bump(l)\n    let got = try size(l)\n    match ok {\n        .ok => match got {\n            .ok(n) => n,\n            .err(_) => 0,\n        },\n        .err(_) => -1,\n    }\n}";
    let c = compile(src);
    validate(&c.wasm);
    let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has("try_invoke") && has("enum_is") && has("enum_get_i"));
}

#[test]
fn try_is_for_flows_and_action_calls() {
    assert_eq!(
        codes(
            "type L {\n    n: Int\n}\naction a(l: L) {\n    l.n = 1\n}\nview v(l: L) {\n    let x = try a(l)\n    text(\"x\")\n}"
        ),
        vec!["E044"]
    );
    assert_eq!(
        codes(
            "fn f() -> Int {\n    return 1\n}\nflow g() -> Int {\n    let x = try f()\n    return 1\n}"
        ),
        vec!["E044"]
    );
    assert_eq!(
        codes("flow g() -> Int {\n    let x = try 1\n    return 1\n}"),
        vec!["E044"]
    );
}
