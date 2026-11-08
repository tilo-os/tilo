//! The ABI additions for sorting, reading maps, boxed set elements and `try`.

use crate::common::{codes, compile_body, compile_body as compile, compile_with, validate};

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
fn an_action_declares_its_run_into_its_metadata() {
    let c = compile_body(
        "type Note {\n    title: Text\n}\n\
action reply(n: Note) (icon: .reply, group: \"Reply\") {\n    n.title = n.title\n}\n\
action archive(n: Note) (icon: .archive, group: \"Reply\") {\n    n.title = n.title\n}\n\
action rename(n: Note) (icon: .pen_tool) {\n    n.title = n.title\n}\n\
view v(n: Note) {\n    text(n.title)\n}",
    );
    let action = |name: &str| {
        c.meta
            .actions
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("{name} is declared"))
    };
    assert_eq!(action("reply").group.as_deref(), Some("Reply"));
    assert_eq!(action("archive").group.as_deref(), Some("Reply"));
    assert_eq!(action("rename").group, None);
    // A surface reads the run by the name the app gave it: a group is text in quotes, and a
    // bare one is refused, so a run is never silently another's.
    assert_eq!(
        codes("type N {\n    t: Text\n}\naction a(n: N) (group: Reply) {\n    n.t = n.t\n}"),
        vec!["E090"]
    );
    // Where an action stands is read off what it is, so a hint at where to put it is not an
    // option of the language.
    assert_eq!(
        codes("type N {\n    t: Text\n}\naction a(n: N) (frequent: true) {\n    n.t = n.t\n}"),
        vec!["E090"]
    );
    assert_eq!(
        codes("type N {\n    t: Text\n}\naction a(n: N) (toolbar: true) {\n    n.t = n.t\n}"),
        vec!["E090"]
    );
}

#[test]
fn resource_reads_a_record_by_its_id_as_whatever_type_it_is() {
    let c = compile_body(
        "type Note {\n    title: Text\n}\n\
type Notebook {\n    notes: Collection<Note>\n}\n\
fn first(record: Text) -> Option<host.Resource> {\n    if record == \"\" { .none } else { resource(record) }\n}\n\
view v(nb: Notebook) {\n    if let shown = first(nb.id()) {\n        text(meta(shown).title)\n    }\n}",
    );
    validate(&c.wasm);
    let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has("resource_ref"), "the view reads a record by its id");
    // A host.Resource has no fields of its own: what it is decides them.
    assert_eq!(
        codes(
            "type N {\n    t: Text\n}\nview v(r: Option<host.Resource>) {\n    if let x = r {\n        text(x.title)\n    }\n}"
        ),
        vec!["E021"]
    );
}

#[test]
fn try_reports_whether_an_invocation_worked() {
    let src = "type L {\n    n: Int merge counter\n}\naction bump(l: L) {\n    l.n += 1\n}\naction size(l: L) -> Int {\n    return l.n\n}\naction f(l: L) -> Int {\n    let ok = try bump(l)\n    let got = try size(l)\n    match ok {\n        .ok => match got {\n            .ok(n) => n,\n            .err(_) => 0,\n        },\n        .err(_) => -1,\n    }\n}";
    let c = compile(src);
    validate(&c.wasm);
    let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has("try_invoke") && has("enum_is") && has("enum_get_i"));
}

#[test]
fn try_is_an_actions_or_a_programs() {
    assert_eq!(
        codes(
            "type L {\n    n: Int\n}\naction a(l: L) {\n    l.n = 1\n}\nview v(l: L) {\n    let x = try a(l)\n    text(\"x\")\n}"
        ),
        vec!["E044"]
    );
    assert_eq!(
        codes(
            "fn f() -> Int {\n    return 1\n}\naction g() -> Int {\n    let x = try f()\n    return 1\n}"
        ),
        vec!["E044"]
    );
    assert_eq!(
        codes("action g() -> Int {\n    let x = try 1\n    return 1\n}"),
        vec!["E044"]
    );
}

#[test]
fn another_packages_fn_is_called_where_it_is_written() {
    let lib = compile_with(
        "app so.tilo.lib \"Lib\"\nfn double(x: Int) -> Int {\n    return x * 2\n}\nfn ping() -> Int {\n    return 1\n}\n",
        &[],
    )
    .unwrap_or_else(|d| panic!("{d:#?}"))
    .meta;
    let c = compile_with(
        "app t.x \"X\"\nuse so.tilo.lib as lib\n\
type T {\n    n: Int\n}\nroot T\ndefault view v for T\n\
fn twice(x: Int) -> Int {\n    return lib.double(lib.double(x))\n}\n\
action sweep(t: T) -> Int {\n    let n = lib.double(3)\n    return n\n}\n\
action bump(t: T) {\n    t.n = twice(2)\n    lib.ping()\n}\n\
view v(t: T) {\n    text(\"{lib.double(t.n)}\")\n}",
        &[lib],
    )
    .unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&c.wasm);
    let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has("call_fn"), "the call crosses packages through call_fn");
}
