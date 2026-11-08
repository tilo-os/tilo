//! `type X = ...`: unions of resource types matched by type, aliases, and roots that are named
//! records holding a collection (`type Notebook { notes: Collection<Note> }`,
//! `root notebook: Notebook`).

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

const CHANNELS: &str = "type TextChannel {\n    name: Text\n}\ntype VoiceChannel {\n    name: Text\n    live: Bool = false\n}\ntype Channel = TextChannel | VoiceChannel\ntype Server {\n    channels: Collection<Channel>\n    pinned: Option<Channel> = .none\n}\nroot Server\ndefault view server for Server\nview server(s: Server) {\n    text(\"x\")\n}\n";

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
fn flat_map_joins_the_lists_a_lambda_gives_into_one() {
    // The conversations a page lists: a group holds channels, a channel or a conversation
    // stands for itself, joined in the order the lists and their items keep. A list of one
    // type is read as the list of the union: items are read, never written back through it.
    let kinds = "type TextChannel {\n    name: Text\n}\ntype VoiceChannel {\n    name: Text\n}\ntype Channel = TextChannel | VoiceChannel\ntype Group {\n    name: Text\n    channels: Collection<TextChannel>\n}\ntype Node = Group | TextChannel | VoiceChannel\ntype Conversations {\n    nodes: Collection<Node>\n}\nroot Conversations\ndefault view conversations for Conversations\nview conversations(c: Conversations) {\n    text(\"x\")\n}\n";
    ok(&format!(
        "{kinds}fn listed(n: Node) -> List<Channel> {{\n    match n {{\n        g: Group => g.channels.filter(ch -> ch.name != \"\"),\n        t: TextChannel => [t],\n        v: VoiceChannel => [v],\n    }}\n}}\nfn all(c: Conversations) -> List<Channel> {{\n    c.nodes.flat_map(n -> listed(n))\n}}"
    ));
    // What the lambda gives is a list to join, not one value.
    expect(
        "E020",
        &format!(
            "{kinds}fn f(c: Conversations) -> List<Text> {{\n    c.nodes.flat_map(n -> \"x\")\n}}"
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
    // A name for a container is no resource: a record type holds it.
    expect(
        "E013",
        "type Note {\n    title: Text\n}\ntype Notes = Collection<Note>",
    );
}

const NOTES: &str = "type Note {\n    title: Text\n    pinned: Bool = false\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nroot notebook: Notebook\ndefault view notebook for Notebook\nview notebook(nb: Notebook) {\n    std.list(nb.notes) { (n, at) ->\n        ui.ListItem(item: n, presence: at)\n    }\n}\n";

#[test]
fn a_root_is_a_named_record_holding_its_collection() {
    ok(&format!(
        "{NOTES}primary action new_note(nb: Notebook) -> Note (icon: .plus) {{\n    nb.notes.insert(Note(title: \"Untitled\"))\n}}\naction clear(nb: Notebook) -> Int {{\n    nb.notes.remove_where(.pinned)\n}}\nfn count() -> Int {{\n    let n = 0\n    for x in notebook.notes {{\n        n += 1\n    }}\n    n + notebook.notes.count(.pinned) + notebook.notes.len()\n}}"
    ));
    let src = format!("app so.tilo.notes \"Notes\"\n{NOTES}");
    let notes = compile_with(&src, &[]).expect("notes").meta;
    assert_eq!(notes.named_roots.len(), 1);
    let notebook = notes
        .types
        .iter()
        .find(|t| t.name == "Notebook")
        .expect("Notebook");
    assert!(notebook.fields.iter().any(|f| f.name == "notes"));
    // Another package reaches it as `alias.name`, and its collection as that field.
    let board = "use so.tilo.notes as notes\ntype Board {\n    n: Int\n}\nroot Board\ndefault view board for Board\nview board(b: Board) {\n    let ns = notes.notebook.notes\n    text(\"{ns.len()} notes\")\n}";
    let got = codes_with(board, std::slice::from_ref(&notes));
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn a_record_keeps_the_ids_of_the_collection_root_it_replaces() {
    // A name for a container was one resource whose one field was `Notes.items`. Renaming the
    // field and then the type in the ids lock (`tilolang ids --rename`) gives the record both
    // ids: the root `root-<id>` and the field's id are the ones the stored records have.
    let mut lock =
        tilolang::IdsLock::parse("Notes = v7cub7aw\nNotes.items = tmqqtkeb\n").expect("lock");
    lock.rename("Notes.items", "Notebook.notes").expect("field");
    lock.rename("Notes", "Notebook").expect("type");
    let src = format!("app so.tilo.notes \"Notes\"\n{NOTES}");
    let compiled =
        tilolang::compile_package_with(&src, &lock, &[], &mut tilolang::SeededEntropy(1))
            .expect("compiles");
    let notes = compiled
        .meta
        .types
        .iter()
        .find(|t| t.name == "Notebook")
        .expect("Notebook");
    assert_eq!(notes.id, "v7cub7aw");
    let field = notes
        .fields
        .iter()
        .find(|f| f.name == "notes")
        .expect("notes");
    assert_eq!(field.id, "tmqqtkeb");
    assert_eq!(compiled.meta.roots, ["v7cub7aw"]);
}

#[test]
fn a_union_draws_its_rows_by_the_row_view_of_each_item_type() {
    // One page over channels that say different things: each item is drawn by the row view of
    // the type it is, so the list's rows all stand as rows.
    let rows = "row view text_row for TextChannel\nview text_row(c: TextChannel) {\n    row(c.name)\n}\nrow view voice_row for VoiceChannel\nview voice_row(c: VoiceChannel) {\n    row(c.name, secondary: \"voice\")\n}\n";
    ok(&format!(
        "{CHANNELS}{rows}view listing(s: Server) {{\n    list(s.channels) {{ c ->\n        show(c, row: true)\n    }}\n}}\nview one(c: Channel) {{\n    show(c, row: true)\n}}"
    ));
    // A type of the union without a row view is named, and so is what to declare for it.
    let only_text =
        "row view text_row for TextChannel\nview text_row(c: TextChannel) {\n    row(c.name)\n}\n";
    let got = codes(&format!(
        "{CHANNELS}{only_text}view listing(s: Server) {{\n    list(s.channels) {{ c ->\n        show(c, row: true)\n    }}\n}}"
    ));
    assert!(got.iter().any(|c| c == "E097"), "got {got:?}");
    // A list of a union writes its rows: a row is the row of the type the item is.
    expect(
        "E097",
        &format!("{CHANNELS}{rows}view listing(s: Server) {{\n    list(s.channels)\n}}"),
    );
}
