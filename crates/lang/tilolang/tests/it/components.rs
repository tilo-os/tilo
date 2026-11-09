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
    title: Text
    body: Text
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

/// The `progress` primitive is the toolkit's `ProgressBar`, not a bar the host draws apart.
#[test]
fn the_progress_primitive_is_the_toolkits_progress_bar() {
    let c = compile_body(&with_note(
        r"view progress_of(n: Note) {
    progress(0.4)
}",
    ));
    validate(&c.wasm);
    let strings = String::from_utf8_lossy(&c.wasm).into_owned();
    assert!(
        strings.contains("ui.ProgressBar"),
        "the node is ui.ProgressBar"
    );
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
fn a_templates_item_block_fills_a_components_part_slot() {
    // The caller's block or view is what the component's slot holds.
    let c = compile_body(&with_note(
        r"view note_row(n: Note) {
    ui.TreeViewRow(checkbox: n.done, secondary: n.title)
}

view outline<T>(notes: List<T>, label: (T) -> Text, row: (T) -> View) {
    ui.TreeView(name: .none, fill: true) {
        for x in notes {
            ui.TreeViewNode(x, label(x)) {
                row: { row(x) }
            }
        }
    }
}

view v(nb: Notebook) {
    outline(nb.notes, x -> x.title, note_row)
    outline(nb.notes, x -> x.title) { x ->
        note_row(x)
    }
}",
    ));
    validate(&c.wasm);
    // A caller block that makes something else is refused where the block is written.
    let (code, message) = one_error(&with_note(
        r"view outline<T>(notes: List<T>, label: (T) -> Text, row: (T) -> View) {
    ui.TreeView(name: .none, fill: true) {
        for x in notes {
            ui.TreeViewNode(x, label(x)) {
                row: { row(x) }
            }
        }
    }
}

view v(nb: Notebook) {
    outline(nb.notes, x -> x.title) { x ->
        ui.Text(x.title)
    }
}",
    ));
    assert_eq!(code, "E063");
    assert!(message.contains("TreeViewRow"), "{message}");
}

#[test]
fn a_binding_binds_a_field_of_its_type() {
    let c = compile_body(&with_note(
        "view v(n: Note) {\n    ui.TextInput(bind: n.title, placeholder: \"Title\")\n    ui.ListRow(title: n.title, bind: n.done)\n}",
    ));
    validate(&c.wasm);
    let (code, _) = one_error(&with_note(
        "view v(n: Note) {\n    ui.TextInput(bind: n.done)\n}",
    ));
    assert_eq!(code, "E062");
    let (code, _) = one_error(&with_note(
        "view v(n: Note) {\n    ui.TextInput(bind: \"fixed\")\n}",
    ));
    assert_eq!(code, "E062");
}

/// A control edits the place it is bound to through `bind:` alone: its own name for that value
/// is no argument, and the error says what to write.
#[test]
fn a_control_edits_a_place_through_bind_alone() {
    for src in [
        "view v(n: Note) {\n    ui.TextInput(value: n.title)\n}",
        "view v(n: Note) {\n    ui.Checkbox(value: n.done)\n}",
        "view v(n: Note) {\n    checkbox(value: n.done)\n}",
        "view v() {\n    state query: Text\n    input(value: query)\n}",
    ] {
        let (code, message) = one_error(&with_note(src));
        assert_eq!(code, "E063", "{src}");
        assert!(message.contains("`bind: "), "{src}: {message}");
    }
    // A value the view does not hold is given to the control it shows it in, not bound: a
    // device's volume, whose change runs the device's own action.
    let c = compile_body(&with_note(
        "view v(n: Note) {\n    ui.Slider(value: 0.5, label: .some(\"Volume\"))\n}",
    ));
    validate(&c.wasm);
}

/// `bind:` names whatever the component's description declares its binding to be, so it is one
/// path for every component: `TextInput`'s `value`, `ListRow`'s `checkbox`; a component with
/// none says so, and a shorthand is held to its component.
#[test]
fn a_binding_binds_the_binding_the_description_declares() {
    let c = compile_body(&with_note(
        "view v(n: Note) {\n    checkbox(bind: n.done)\n    ui.TextInput(bind: n.title)\n    ui.ListRow(title: n.title, bind: n.done)\n}",
    ));
    validate(&c.wasm);
    let strings = String::from_utf8_lossy(&c.wasm).into_owned();
    for want in ["ui.Checkbox", "ui.TextInput", "bind", "value"] {
        assert!(strings.contains(want), "{want} reaches the host");
    }
    let (code, message) = one_error("view v() {\n    ui.Section(\"About\", bind: \"x\")\n}");
    assert_eq!(code, "E063");
    assert!(message.contains("binds nothing"), "{message}");
    let (code, message) = one_error("view v() {\n    checkbox(3)\n}");
    assert_eq!(code, "E020");
    assert!(message.contains("Text"), "{message}");
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
    text: Text
    reactions: Map<List<Principal>>
}

type Conversation {
    title: Text
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
    text: Text
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
fn an_action_list_prop_takes_the_actions_the_host_offers() {
    // A view never reads the actions back, so it cannot make one: the host fills the prop.
    let (code, message) = one_error(&format!(
        "{CHAT}\nview more(m: Message) {{\n    ui.MenuButton(\"More\", actions: actions(m))\n}}"
    ));
    assert_eq!(code, "E047");
    assert!(message.contains("host"), "{message}");
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
    let body = "type Note {\n    title: Text\n    read: Bool = false\n}\ntype Notebook {\n    notes: Collection<Note>\n}\naction read(n: Note) {\n    n.read = true\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(n, front: false).then(read(n))\n    }\n}";
    assert!(errors(body).is_empty(), "{:#?}", errors(body));
    let reads_other = "type Note {\n    title: Text\n}\ntype Notebook {\n    notes: Collection<Note>\n}\naction keep(nb: Notebook, title: Text) {\n    nb.notes.insert(Note(title: title))\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(n).then(keep(nb, n.title))\n    }\n}";
    assert_eq!(one_error(reads_other).0, "E063");
}

/// components.md §4.1: a drawing's shapes are cases of the toolkit's `ui.Shape`, written like
/// any enum's, their inks the design system's roles; a field a shape does not have is refused.
#[test]
fn a_drawing_is_shapes_in_the_design_systems_roles() {
    let c = compile_body(&with_note(
        r"view usage(n: Note) {
    let used = if n.done { 1.0 } else { 0.6 }
    ui.Drawing([
        .arc(cx: 0.5, cy: 0.5, radius: 0.4, from: 0.0, to: 1.0, stroke: .track),
        .arc(cx: 0.5, cy: 0.5, radius: 0.4, from: 0.0, to: used, stroke: .accent, weight: .strong),
    ], meaning: .progress, value: used)
}",
    ));
    validate(&c.wasm);
    let strings = String::from_utf8_lossy(&c.wasm).into_owned();
    assert!(
        strings.contains("ui.Drawing"),
        "the node is the toolkit's Drawing"
    );

    let got = errors(&with_note(
        r"view usage(n: Note) {
    ui.Drawing([.arc(cx: 0.5, cy: 0.5, radius: 0.4, from: 0.0, to: 1.0, stroke: .crimson)], meaning: .progress)
}",
    ));
    assert!(
        got.iter()
            .any(|(_, m)| m.contains("`.crimson`") && m.contains(".accent")),
        "a colour that is no role is refused, naming the roles: {got:#?}"
    );
    let got = errors(&with_note(
        r"view usage(n: Note) {
    ui.Drawing([.arc(cx: 0.5, cy: 0.5, radius: 0.4, from: 0.0, to: 1.0, stroke: .accent, blur: 3.0)], meaning: .progress)
}",
    ));
    assert!(
        got.iter().any(|(_, m)| m.contains("radius")),
        "a field the shape does not have is refused, naming its fields: {got:#?}"
    );
}
