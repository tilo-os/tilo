//! components.md, "A component is a described widget": the toolkit's components, read as the
//! package `ui`, checked against the descriptions the toolkit generates from each widget.

use crate::common::{compile_body, validate};

/// The diagnostics of a module body, as (code, message).
fn errors(body: &str) -> Vec<(String, String)> {
    let src = format!("app t.x \"X\"\n{body}\n");
    match crate::common::compile_with(&src, &[]) {
        Ok(_) => Vec::new(),
        Err(d) => d.into_iter().map(|d| (d.code, d.message)).collect(),
    }
}

/// The one error of `body`: its code and its message.
fn one_error(body: &str) -> (String, String) {
    let got = errors(body);
    assert_eq!(got.len(), 1, "one error for:\n{body}\ngot {got:#?}");
    got.into_iter().next().unwrap()
}

const NOTE: &str = r"type Note {
    title: Text (title)
    body: Text (body)
    pinned: Bool = false
    done: Bool = false
}

type Notebook {
    notes: Collection<Note>
}

action open_note(n: Note) {
    n.pinned = n.pinned
}
";

fn with_note(view: &str) -> String {
    format!("{NOTE}\n{view}")
}

#[test]
fn a_component_call_compiles_to_its_node_with_props_slots_and_events() {
    let c = compile_body(&with_note(
        r#"view row_of(n: Note) {
    ui.ListRow(title: n.title, secondary: n.body, mark: if n.pinned { .pin }) {
        avatar: { ui.Avatar(name: n.title, glyph: .users) }
        trailing: { ui.Text("now", role: .caption) }
        on activate: open_note(n)
    }
}"#,
    ));
    validate(&c.wasm);
    let strings = String::from_utf8_lossy(&c.wasm).into_owned();
    for want in [
        "ui.ListRow",
        "ui.Avatar",
        "ui.Text",
        "slot",
        "trailing",
        "activate",
        "caption",
    ] {
        assert!(strings.contains(want), "{want} reaches the host");
    }
}

#[test]
fn positional_props_follow_the_description() {
    let c = compile_body(&with_note(
        r#"view title_of(n: Note) {
    ui.Section("About") {
        ui.Text(n.title, role: .title)
        ui.PropertyList {
            ui.Property("Title", n.title)
        }
    }
}"#,
    ));
    validate(&c.wasm);
}

#[test]
fn a_prop_of_the_wrong_type_is_named_with_the_widgets_doc_text() {
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title, secondary: 3)\n}",
    ));
    assert_eq!(code, "E020");
    assert!(
        message.contains("`ui.ListRow`'s `secondary` is Option<Text>"),
        "{message}"
    );
    assert!(
        message.contains("the start of the item's content"),
        "the prop's doc text: {message}"
    );
    assert!(message.contains("found Int"), "{message}");
}

#[test]
fn a_missing_required_prop_is_named_with_its_doc_text() {
    let (code, message) = one_error("view v() {\n    ui.EmptyState(icon: .plus)\n}");
    assert_eq!(code, "E022");
    assert!(
        message.contains("`ui.EmptyState` needs `title`"),
        "{message}"
    );
    assert!(message.contains("one short sentence"), "{message}");
}

#[test]
fn an_unknown_argument_lists_the_components_arguments() {
    let (code, message) = one_error("view v() {\n    ui.Text(\"x\", colour: \"red\")\n}");
    assert_eq!(code, "E063");
    assert!(message.contains("has no argument `colour`"), "{message}");
    assert!(message.contains("value, role, numeric"), "{message}");
}

#[test]
fn an_event_given_as_an_argument_is_pointed_to_the_block() {
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title, activate: open_note(n))\n}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("on activate:"), "{message}");
}

#[test]
fn a_case_outside_the_design_systems_roles_is_refused() {
    let (code, message) = one_error("view v() {\n    ui.Text(\"x\", role: .shouty)\n}");
    assert_eq!(code, "E065");
    assert!(
        message.contains("TextRole has no case `.shouty`"),
        "{message}"
    );
    assert!(
        message.contains(".caption"),
        "the cases are listed: {message}"
    );
}

#[test]
fn an_unknown_component_slot_or_event_is_refused() {
    let (code, message) = one_error("view v() {\n    ui.Gauge(3)\n}");
    assert_eq!(code, "E060");
    assert!(
        message.contains("ListRow"),
        "the components are listed: {message}"
    );
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title) {\n        footer: { ui.Text(\"x\") }\n    }\n}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("no slot `footer`"), "{message}");
    assert!(
        message.contains("`trailing`"),
        "the slots are listed: {message}"
    );
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title) {\n        on swipe: open_note(n)\n    }\n}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("no event `swipe`"), "{message}");
}

#[test]
fn a_slot_that_accepts_one_component_refuses_others() {
    let (code, message) = one_error(&with_note(
        "view v(n: Note) {\n    ui.ListRow(title: n.title) {\n        avatar: { ui.Text(n.title) }\n    }\n}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("holds `ui.Avatar` only"), "{message}");
    let (code, _) =
        one_error("view v() {\n    ui.PropertyList {\n        ui.Text(\"not a fact\")\n    }\n}");
    assert_eq!(code, "E063");
}

#[test]
fn a_component_without_content_of_its_own_takes_no_block_content() {
    let (code, message) =
        one_error("view v() {\n    ui.Text(\"x\") {\n        ui.Text(\"y\")\n    }\n}");
    assert_eq!(code, "E063");
    assert!(message.contains("holds no content of its own"), "{message}");
}

#[test]
fn a_binding_binds_a_field_of_its_type() {
    let c = compile_body(&with_note(
        "view v(n: Note) {\n    ui.TextInput(value: n.title, placeholder: \"Title\")\n    ui.ListRow(title: n.title, checkbox: n.done)\n}",
    ));
    validate(&c.wasm);
    let (code, _) = one_error(&with_note(
        "view v(n: Note) {\n    ui.TextInput(value: n.done)\n}",
    ));
    assert_eq!(code, "E062");
    let (code, _) = one_error(&with_note(
        "view v(n: Note) {\n    ui.TextInput(value: \"fixed\")\n}",
    ));
    assert_eq!(code, "E062");
}

#[test]
fn a_row_view_may_be_the_toolkits_row_itself() {
    let c = compile_body(&with_note(
        "row view note_row for Note\n\nview note_row(n: Note) {\n    ui.ListRow(title: n.title, secondary: n.body)\n}",
    ));
    validate(&c.wasm);
}

#[test]
fn slots_and_events_print_back_as_written() {
    let src = "app t.x \"X\"\n\naction go() {\n}\n\nview v() {\n    ui.Card(title: \"Today\") {\n        ui.Text(\"Three things\")\n        on activate: go()\n    }\n    ui.ListRow(title: \"Row\") {\n        trailing: {\n            ui.Text(\"now\")\n        }\n    }\n}\n";
    let module = tilolang::parse(src).expect("parses");
    let printed = tilolang::print_module(&module);
    assert!(printed.contains("on activate: go()"), "{printed}");
    assert!(printed.contains("trailing: {"), "{printed}");
    let again = tilolang::parse(&printed).expect("the printed module parses");
    assert_eq!(
        tilolang::print_module(&again),
        printed,
        "printing is stable"
    );
}

const CHAT: &str = r#"type Message {
    text: Text (title)
    reactions: Map<List<Principal>>
}

type Conversation {
    title: Text (title)
    messages: Collection<Message>
}

action pin(m: Message) (label: "Pin", icon: .pin) {
    m.text = "pinned"
}
"#;

#[test]
fn a_map_field_is_a_map_prop_whole_in_a_view() {
    let c = compile_body(&format!(
        "{CHAT}\nview reactions_of(m: Message) {{\n    ui.Reactions(reactions: m.reactions)\n}}"
    ));
    validate(&c.wasm);
    let (code, message) = one_error(
        "type Tally {\n    counts: Map<Int>\n}\nview tally(t: Tally) {\n    ui.Reactions(reactions: t.counts)\n}",
    );
    assert_eq!(code, "E020");
    assert!(
        message.contains("Map<Text, Set<Principal>>") && message.contains("Map<Int>"),
        "{message}"
    );
    let (code, _) = one_error(&format!(
        "{CHAT}\nfn whole(m: Message) -> Int {{\n    let r = m.reactions\n    1\n}}"
    ));
    assert_eq!(code, "E080", "outside a view a map is read by key");
}

#[test]
fn the_older_reactions_form_still_compiles() {
    let c = compile_body("view old() {\n    ui.Reactions([\"👍\"], can_react: false)\n}");
    validate(&c.wasm);
}

const REACTED: &str = r"type Post {
    text: Text (title)
    reactions: Map<Set<Principal>>
}
";

#[test]
fn a_map_of_sets_takes_and_gives_back_elements_one_by_one() {
    let c = compile_body(&format!(
        "{REACTED}
action react(p: Post, emoji: Text) {{
    let mine = if let who = p.reactions.get(emoji) {{
        who.any(x -> x == invoker)
    }} else {{
        false
    }}
    if mine {{
        p.reactions.remove(emoji, invoker)
    }} else {{
        p.reactions.add(emoji, invoker)
    }}
}}

action clear(p: Post, emoji: Text) {{
    p.reactions.remove(emoji)
}}

view shown(p: Post) {{
    ui.Reactions(reactions: p.reactions)
}}"
    ));
    validate(&c.wasm);
    let strings = String::from_utf8_lossy(&c.wasm).into_owned();
    assert!(strings.contains("map_set_add") && strings.contains("map_set_remove"));
    let (code, message) = one_error(&format!(
        "{REACTED}\naction all(p: Post) {{\n    p.reactions.put(\"x\", [invoker])\n}}"
    ));
    assert_eq!(code, "E027");
    assert!(message.contains("map of sets"), "{message}");
}

#[test]
fn an_action_list_is_an_action_prop_run_by_events_of_the_node() {
    let c = compile_body(&format!(
        "{CHAT}\nview more(m: Message) {{\n    ui.MenuButton(\"More\", actions: actions(m))\n}}"
    ));
    validate(&c.wasm);
    let strings = String::from_utf8_lossy(&c.wasm).into_owned();
    assert!(
        strings.contains("ui_actions"),
        "the actions reach the host as offered"
    );
    let (code, message) = one_error(&format!(
        "{CHAT}\nview more(m: Message) {{\n    ui.MenuButton(\"More\", actions: [m.text])\n}}"
    ));
    assert_eq!(code, "E020");
    assert!(message.contains("List<Action>"), "{message}");
}

#[test]
fn collections_and_maps_are_view_parameters() {
    let c = compile_body(&format!(
        "{CHAT}
view lines(items: Collection<Message>) {{
    for m in items {{
        text(m.text)
    }}
}}

view tally(r: Map<List<Principal>>) {{
    ui.Reactions(reactions: r)
}}

view thread(c: Conversation) {{
    lines(c.messages)
    if let m = c.messages.last() {{
        tally(m.reactions)
    }}
}}"
    ));
    validate(&c.wasm);
    let (code, _) = one_error(&format!(
        "{CHAT}\nfn count(items: Collection<Message>) -> Int {{\n    items.len()\n}}"
    ));
    assert_eq!(code, "E023", "only a view takes a collection as it is");
}

#[test]
fn a_collection_is_no_component_prop_and_goes_to_a_view_instead() {
    let (code, message) = one_error(&format!(
        "{CHAT}\nview bar(c: Conversation) {{\n    ui.EmojiBar(c.messages)\n}}"
    ));
    assert_eq!(code, "E020");
    assert!(
        message.contains("List<Text>") && message.contains("Collection<T>"),
        "{message}"
    );
}

#[test]
fn an_events_value_reaches_every_step_of_a_then_chain_that_uses_it() {
    let body = "type Note {\n    title: Text (title)\n    read: Bool = false\n}\ntype Notebook {\n    notes: Collection<Note>\n}\naction read(n: Note) {\n    n.read = true\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(n, front: false).then(read(n))\n    }\n}";
    assert!(errors(body).is_empty(), "{:#?}", errors(body));
    let reads_other = "type Note {\n    title: Text (title)\n}\ntype Notebook {\n    notes: Collection<Note>\n}\naction keep(nb: Notebook, title: Text) {\n    nb.notes.insert(Note(title: title))\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(n).then(keep(nb, n.title))\n    }\n}";
    assert_eq!(one_error(reads_other).0, "E063");
}
