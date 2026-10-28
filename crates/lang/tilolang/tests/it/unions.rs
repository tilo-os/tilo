//! `type X = ...`: unions of resource types matched by type, aliases, and roots that are named
//! values of a type naming a container (`type Notes = Collection<Note>`, `root notes: Notes`).

use crate::common::{codes, codes_with, compile_with, validate};

fn ok(body: &str) {
    let src = format!("app t.x \"X\"\n{body}\n");
    let compiled = compile_with(&src, &[]).unwrap_or_else(|d| panic!("{body}\n{d:#?}"));
    validate(&compiled.wasm);
    let printed = tilolang::print_module(&tilolang::parse(&src).expect("parses"));
    let again = tilolang::print_module(&tilolang::parse(&printed).expect("reparses"));
    assert_eq!(printed, again);
}

fn expect(code: &str, body: &str) {
    let got = codes(body);
    assert!(
        got.iter().any(|c| c == code),
        "expected {code} for:\n{body}\ngot {got:?}"
    );
}

const CHANNELS: &str = "type TextChannel {\n    name: Text (title)\n}\ntype VoiceChannel {\n    name: Text (title)\n    live: Bool = false\n}\ntype Channel = TextChannel | VoiceChannel\ntype Server {\n    channels: Collection<Channel>\n    pinned: Option<Channel> = .none\n}\nroot Server\ndefault view server for Server\nview server(s: Server) {\n    text(\"x\")\n}\n";

#[test]
fn a_union_holds_any_of_its_types_and_a_match_takes_it_apart_by_type() {
    ok(&format!(
        "{CHANNELS}fn label(c: Channel) -> Text {{\n    match c {{\n        t: TextChannel => \"# {{t.name}}\",\n        v: VoiceChannel => if v.live {{ \"live {{v.name}}\" }} else {{ v.name }},\n    }}\n}}\naction add(s: Server, name: Text) {{\n    s.channels.insert(TextChannel(name: name))\n    s.channels.insert(VoiceChannel(name: name))\n}}\naction pin(s: Server, c: Channel) {{\n    s.pinned = c\n}}\nview lines(s: Server) {{\n    stack {{\n        for c in s.channels {{\n            match c {{\n                t: TextChannel => text(t.name),\n                v: VoiceChannel => text(v.name),\n            }}\n        }}\n    }}\n}}"
    ));
    // Every type has an arm.
    expect(
        "E035",
        &format!(
            "{CHANNELS}fn label(c: Channel) -> Text {{\n    match c {{\n        t: TextChannel => t.name,\n    }}\n}}"
        ),
    );
    // A field depends on which type it is.
    expect(
        "E021",
        &format!("{CHANNELS}fn label(c: Channel) -> Text {{\n    c.name\n}}"),
    );
    // A case is a resource type.
    expect("E109", "type A {\n    n: Int\n}\ntype U = A | Int");
    expect(
        "E036",
        &format!(
            "{CHANNELS}type Other {{\n    n: Int\n}}\nfn label(c: Channel) -> Text {{\n    match c {{\n        t: TextChannel => t.name,\n        o: Other => \"\",\n    }}\n}}"
        ),
    );
}

#[test]
fn enum_cases_hold_no_data() {
    ok("enum Order {\n    created\n    title\n}");
    expect("E110", "enum Shape {\n    dot\n    line(len: Int)\n}");
}

#[test]
fn an_alias_is_another_name_for_a_type() {
    ok(
        "type Note {\n    title: Text\n}\ntype Notes = List<Note>\nfn first(ns: Notes) -> Option<Note> {\n    ns.first()\n}",
    );
    expect("E013", "type A = B\ntype B = A");
}

const NOTES: &str = "type Note {\n    title: Text (title)\n    pinned: Bool = false\n}\ntype Notes = Collection<Note>\nroot notes: Notes\ndefault view notebook for notes\nview notebook(ns: Notes) {\n    std.list(ns, ns, empty: \"No notes\") { n ->\n        row(n.title)\n    }\n}\n";

#[test]
fn a_root_is_a_named_resource_of_a_type_that_names_a_container() {
    ok(&format!(
        "{NOTES}primary action new_note(ns: Notes) -> Note (icon: .plus) {{\n    ns.insert(Note(title: \"Untitled\"))\n}}\naction clear(ns: Notes) -> Int {{\n    ns.remove_where(.pinned)\n}}\nfn count() -> Int {{\n    let n = 0\n    for x in notes {{\n        n += 1\n    }}\n    n + notes.count(.pinned) + notes.len()\n}}"
    ));
    let src = format!("app so.tilo.notes \"Notes\"\n{NOTES}");
    let notes = compile_with(&src, &[]).expect("notes").meta;
    assert_eq!(notes.named_roots.len(), 1);
    assert!(notes.types.iter().any(|t| t.holder && t.name == "Notes"));
    // Another package reaches it as `alias.name`.
    let board = "use so.tilo.notes as notes\ntype Board {\n    n: Int\n}\nroot Board\ndefault view board for Board\nview board(b: Board) {\n    let ns = notes.notes\n    text(\"{ns.len()} notes\")\n}";
    let got = codes_with(board, std::slice::from_ref(&notes));
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn a_holder_keeps_the_ids_of_the_type_it_replaces() {
    // Renaming `Notebook` to `Notes` and its field to `Notes.items` in the ids lock keeps the
    // records: the root `root-<id>` and the field's id are the same.
    let lock =
        tilolang::IdsLock::parse("Notes = v7cub7aw\nNotes.items = tmqqtkeb\n").expect("lock");
    let src = format!("app so.tilo.notes \"Notes\"\n{NOTES}");
    let compiled =
        tilolang::compile_package_with(&src, &lock, &[], &mut tilolang::SeededEntropy(1))
            .expect("compiles");
    let notes = compiled
        .meta
        .types
        .iter()
        .find(|t| t.name == "Notes")
        .expect("Notes");
    assert_eq!(notes.id, "v7cub7aw");
    assert_eq!(notes.fields[0].id, "tmqqtkeb");
    assert_eq!(compiled.meta.roots, ["v7cub7aw"]);
}
