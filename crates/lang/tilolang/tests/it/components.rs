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

tilo_test_env::scenario! {
    /// A component call, and what the checker says of a wrong one.
    fn a_component_call_is_checked_against_the_toolkits_description;

    phase a_component_call_compiles_to_its_node_with_props_parts_and_events;
    {
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
            "part",
            "trailing",
            "activate",
            "caption",
        ] {
            assert!(strings.contains(want), "{want} reaches the host");
        }
    }

    phase the_progress_primitive_is_the_toolkits_progress_bar;
    // The `progress` primitive is the toolkit's `ProgressBar`, not a bar the host draws apart.
    {
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

    phase positional_props_follow_the_description;
    {
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

    phase a_prop_of_the_wrong_type_is_named_with_the_widgets_doc_text;
    {
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

    phase a_missing_required_prop_is_named_with_its_doc_text;
    {
        let (code, message) = one_error("view v() {\n    ui.EmptyState(icon: .plus)\n}");
        assert_eq!(code, "E022");
        assert!(
            message.contains("`ui.EmptyState` needs `title`"),
            "{message}"
        );
        assert!(message.contains("one short sentence"), "{message}");
    }

    phase an_unknown_argument_lists_the_components_arguments;
    {
        let (code, message) = one_error("view v() {\n    ui.Text(\"x\", colour: \"red\")\n}");
        assert_eq!(code, "E063");
        assert!(message.contains("has no argument `colour`"), "{message}");
        assert!(message.contains("value, role, numeric"), "{message}");
    }

    phase an_event_given_as_an_argument_is_pointed_to_the_block;
    {
        let (code, message) = one_error(&with_note(
            "view v(n: Note) {\n    ui.ListRow(title: n.title, activate: open_note(n))\n}",
        ));
        assert_eq!(code, "E063");
        assert!(message.contains("on activate:"), "{message}");
    }

    phase a_case_outside_the_design_systems_roles_is_refused;
    {
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

    phase an_unknown_component_part_or_event_is_refused;
    {
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
        assert!(message.contains("no part `footer`"), "{message}");
        assert!(
            message.contains("`trailing`"),
            "the parts are listed: {message}"
        );
        let (code, message) = one_error(&with_note(
            "view v(n: Note) {\n    ui.ListRow(title: n.title) {\n        on swipe: open_note(n)\n    }\n}",
        ));
        assert_eq!(code, "E063");
        assert!(message.contains("no event `swipe`"), "{message}");
    }

    phase a_part_that_accepts_one_component_refuses_others;
    {
        let (code, message) = one_error(&with_note(
            "view v(n: Note) {\n    ui.ListRow(title: n.title) {\n        avatar: { ui.Text(n.title) }\n    }\n}",
        ));
        assert_eq!(code, "E063");
        assert!(message.contains("holds `ui.Avatar` only"), "{message}");
        let (code, _) =
            one_error("view v() {\n    ui.PropertyList {\n        ui.Text(\"not a fact\")\n    }\n}");
        assert_eq!(code, "E063");
    }

    phase a_component_without_content_of_its_own_takes_no_block_content;
    {
        let (code, message) =
            one_error("view v() {\n    ui.Text(\"x\") {\n        ui.Text(\"y\")\n    }\n}");
        assert_eq!(code, "E063");
        assert!(message.contains("holds no content of its own"), "{message}");
    }
}

tilo_test_env::scenario! {
    /// Parts, bindings and rows of a component.
    fn parts_bindings_and_rows_follow_the_description;

    phase a_templates_item_block_fills_a_components_part_part;
    {
        // The caller's block or view is what the component's part holds.
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

    phase a_binding_binds_a_field_of_its_type;
    {
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

    phase a_control_edits_a_place_through_bind_alone;
    // A control edits the place it is bound to through `bind:` alone: its own name for that value
    // is no argument, and the error says what to write.
    {
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

    phase a_binding_binds_the_binding_the_description_declares;
    // `bind:` names whatever the component's description declares its binding to be, so it is one
    // path for every component: `TextInput`'s `value`, `ListRow`'s `checkbox`; a component with
    // none says so, and a shorthand is held to its component.
    {
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

    phase a_row_view_may_be_the_toolkits_row_itself;
    {
        let c = compile_body(&with_note(
            "row view note_row for Note\n\nview note_row(n: Note) {\n    ui.ListRow(title: n.title, secondary: n.body)\n}",
        ));
        validate(&c.wasm);
    }

    phase parts_and_events_print_back_as_written;
    {
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
}

const CHAT: &str = r#"type Message {
    text: Text
    reactions: Map<List<Actor>>
}

type Conversation {
    title: Text
    messages: Collection<Message>
}

action pin(m: Message) (label: "Pin", icon: .pin) {
    m.text = "pinned"
}
"#;

tilo_test_env::scenario! {
    /// What a component takes whole: maps, collections, lists of actions, an event's value and a drawing.
    fn maps_collections_action_lists_events_and_drawings_reach_a_component;

    phase a_map_field_is_a_map_prop_whole_in_a_view;
    {
        let c = compile_body(&format!(
            "{CHAT}\nview reactions_of(m: Message) {{\n    ui.Reactions(reactions: m.reactions)\n}}"
        ));
        validate(&c.wasm);
        let (code, message) = one_error(
            "type Tally {\n    counts: Map<Int>\n}\nview tally(t: Tally) {\n    ui.Reactions(reactions: t.counts)\n}",
        );
        assert_eq!(code, "E020");
        assert!(
            message.contains("Map<Text, Set<Actor>>") && message.contains("Map<Int>"),
            "{message}"
        );
        let (code, _) = one_error(&format!(
            "{CHAT}\nfn whole(m: Message) -> Int {{\n    let r = m.reactions\n    1\n}}"
        ));
        assert_eq!(code, "E080", "outside a view a map is read by key");
    }

    phase the_older_reactions_form_still_compiles;
    {
        let c = compile_body("view old() {\n    ui.Reactions([\"👍\"], can_react: false)\n}");
        validate(&c.wasm);
    }

    phase a_map_of_sets_takes_and_gives_back_elements_one_by_one;
    {
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

    phase an_action_list_prop_takes_the_actions_the_host_offers;
    {
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

    phase collections_and_maps_are_view_parameters;
    {
        let c = compile_body(&format!(
            "{CHAT}
view lines(items: Collection<Message>) {{
    for m in items {{
        text(m.text)
    }}
}}

view tally(r: Map<List<Actor>>) {{
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

    phase a_collection_is_no_component_prop_and_goes_to_a_view_instead;
    {
        let (code, message) = one_error(&format!(
            "{CHAT}\nview bar(c: Conversation) {{\n    ui.EmojiBar(c.messages)\n}}"
        ));
        assert_eq!(code, "E020");
        assert!(
            message.contains("List<Text>") && message.contains("Collection<T>"),
            "{message}"
        );
    }

    phase an_events_value_reaches_every_step_of_a_then_chain_that_uses_it;
    {
        let body = "type Note {\n    title: Text\n    read: Bool = false\n}\ntype Notebook {\n    notes: Collection<Note>\n}\naction read(n: Note) {\n    n.read = true\n}\ndefault view note for Note\nview note(n: Note) {\n    text(n.title)\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(n, front: false).then(read(n))\n    }\n}";
        assert!(errors(body).is_empty(), "{:#?}", errors(body));
        let reads_other = "type Note {\n    title: Text\n}\ntype Notebook {\n    notes: Collection<Note>\n}\naction keep(nb: Notebook, title: Text) {\n    nb.notes.insert(Note(title: title))\n}\ndefault view note for Note\nview note(n: Note) {\n    text(n.title)\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(n).then(keep(nb, n.title))\n    }\n}";
        assert_eq!(one_error(reads_other).0, "E063");
    }

    phase a_drawing_is_shapes_in_the_design_systems_roles;
    // components.md §4.1: a drawing's shapes are cases of the toolkit's `ui.Shape`, written like
    // any enum's, their inks the design system's roles; a field a shape does not have is refused.
    {
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

    phase a_drawing_sets_text_in_type_roles_and_answers_the_pointer;
    // components.md §4.1: a text is a shape set in a theme's type role, and `hover` carries the
    // index of the shape under the pointer, or none.
    {
        let c = compile_body(&with_note(
            r#"view usage(n: Note) {
    state over: Option<Int> = .none
    ui.Drawing([
        .arc(cx: 0.5, cy: 0.5, radius: 0.4, from: 0.0, to: 0.6, stroke: .accent),
        .text(x: 0.5, y: 0.5, text: "60%", role: .title, strong: true),
        .text(x: 0.5, y: 0.7, text: n.title, role: .caption, ink: .muted, anchor: .start),
    ], meaning: .progress, value: .some(0.6), motion: .calm) {
        on hover(at): set(over, at)
    }
}"#,
        ));
        validate(&c.wasm);
        let got = errors(&with_note(
            r#"view usage(n: Note) {
    ui.Drawing([.text(x: 0.5, y: 0.5, text: "60%", font: "Comic Sans")], meaning: .picture)
}"#,
        ));
        assert!(
            got.iter().any(|(_, m)| m.contains("role")),
            "a text names a type role, never a font: {got:#?}"
        );
    }

    phase a_chart_and_a_graph_are_drawn_from_values;
    // A chart's lines and a graph's connections are cases of the toolkit's `ui.Series` and
    // `ui.Connection`; a graph's nodes are views of the caller's.
    {
        let c = compile_body(&with_note(
            r#"view stats(n: Note) {
    ui.Chart([
        .series(values: [1.0, 3.0, 2.0], label: "Reads"),
        .series(values: [0.0, 1.0, 1.0], label: "Writes", ink: .warning),
    ], title: .some(n.title), area: true)
    ui.GraphCanvas(connections: [.connection(from: "a", to: "b")]) {
        ui.GraphNode("a", x: .some(0.0), y: .some(0.0)) {
            ui.Text("Fetch")
        }
        ui.GraphNode("b", x: .some(200.0), y: .some(40.0)) {
            ui.Text("Build")
        }
    }
}"#,
        ));
        validate(&c.wasm);
        let strings = String::from_utf8_lossy(&c.wasm).into_owned();
        assert!(strings.contains("ui.Chart") && strings.contains("ui.GraphCanvas"));
    }

    phase bars_and_a_pie_are_drawn_from_values;
    // A bar chart's bars and a pie's slices are cases of the toolkit's `ui.Bars` and `ui.Slice`: a
    // view gives readings and what they are of, never a colour or a size.
    {
        let c = compile_body(&with_note(
            r#"view stats(n: Note) {
    ui.BarChart(
        [
            .bars(values: [1.0, 3.0, 2.0], label: "Done"),
            .bars(values: [0.0, 1.0, 1.0], label: "Opened"),
        ],
        categories: ["Mon", "Tue", "Wed"],
        title: .some(n.title),
        layout: .stacked,
    )
    ui.PieChart(
        [.slice(label: "Open", value: 3.0), .slice(label: "Done", value: 5.0)],
        ring: true,
        total: .some("8"),
    )
}"#,
        ));
        validate(&c.wasm);
        let strings = String::from_utf8_lossy(&c.wasm).into_owned();
        assert!(strings.contains("ui.BarChart") && strings.contains("ui.PieChart"));
        let got = errors(&with_note(
            r#"view stats(n: Note) {
    ui.PieChart([.slice(label: "Open", value: 3.0, color: "red")])
}"#,
        ));
        assert!(!got.is_empty(), "a slice is given no colour: {got:#?}");
        let got = errors(&with_note(
            r#"view stats(n: Note) {
    ui.BarChart([.bars(values: [1.0])], categories: ["Mon"], height: .some(900.0))
}"#,
        ));
        assert!(!got.is_empty(), "a bar chart is given no size: {got:#?}");
    }

    phase every_shape_chart_is_placed_from_values;
    // A distribution's samples, a polar area's directions, a radar's axes and shapes, and a
    // funnel's stages are cases of the toolkit's own sets: a view gives readings and where they
    // lie, never a colour or a size.
    {
        let c = compile_body(&with_note(
            r#"view stats(n: Note) {
    ui.DistributionChart(
        [
            .samples(values: [4.0, 5.0, 6.0], label: "Before"),
            .samples(values: [3.0, 4.0, 3.5], label: "After"),
        ],
        title: .some(n.title),
        kind: .violin,
    )
    ui.PolarAreaChart(
        [.area(label: "Jan", value: 12.0), .area(label: "Feb", value: 9.0)],
        title: .some("Sign-ups"),
    )
    ui.RadarChart(
        [
            .axis(label: "Speed", max: 100.0),
            .axis(label: "Cost", max: 30.0),
            .axis(label: "Quality", max: 100.0),
        ],
        series: [.series(values: [70.0, 12.0, 88.0], label: "This week")],
    )
    ui.FunnelChart(
        [
            .stage(label: "Visited", value: 1200.0),
            .stage(label: "Signed up", value: 340.0),
        ],
        direction: .pyramid,
    )
}"#,
        ));
        validate(&c.wasm);
        let strings = String::from_utf8_lossy(&c.wasm).into_owned();
        for name in [
            "ui.DistributionChart",
            "ui.PolarAreaChart",
            "ui.RadarChart",
            "ui.FunnelChart",
        ] {
            assert!(strings.contains(name), "{name} is placed");
        }
        let got = errors(&with_note(
            r#"view stats(n: Note) {
    ui.FunnelChart([.stage(label: "Visited", value: 1200.0, color: "red")])
}"#,
        ));
        assert!(!got.is_empty(), "a stage is given no colour: {got:#?}");
    }

    phase a_list_maps_with_each_items_place;
    // `map` and `flat_map` give the lambda the item's place from 0 as its second name, so a
    // drawing lays its shapes out by where each reading stands.
    {
        let c = compile_body(&with_note(
            r"fn bars(xs: List<Float>) -> List<ui.Shape> {
    let width = 1.0 / xs.len().max(1).to_float()
    xs.map((x, i) -> ui.Shape.rect(x: i.to_float() * width, y: 1.0 - x, w: width, h: x, fill: .some(.accent)))
}
view usage(n: Note) {
    ui.Drawing(bars([0.2, 0.8, 0.5]), meaning: .chart)
}",
        ));
        validate(&c.wasm);
        let got = errors(&with_note(
            r"fn odd(xs: List<Int>) -> Bool {
    xs.any((x, i) -> i > 1)
}",
        ));
        assert!(
            got.iter().any(|(code, _)| code == "E051"),
            "only `map` and `flat_map` take a place: {got:#?}"
        );
        let got = errors(&with_note(
            r"fn twice(xs: List<Int>) -> List<Int> {
    xs.map((x, i, j) -> x)
}",
        ));
        assert!(
            got.iter().any(|(code, m)| code == "E051" && m.contains("(x, i)")),
            "a map's message names the place it can take: {got:#?}"
        );
    }
}

tilo_test_env::scenario! {
    /// components.md §2: a view is a component its caller fills: named options with defaults, a
    /// block that fills its view parameters, `on` clauses that answer its action parameters, and
    /// view state of its own.
    fn a_view_is_a_component_its_caller_fills;

    phase options_take_their_defaults_and_the_block_fills_the_slots;
    {
        let c = compile_body(&with_note(
            r#"view person(name: Text, role: Text = "Member", online: Bool = false, extra: View, aside: View, on_message: Option<Action> = .none) {
    state open: Bool = false
    ui.Card(title: name) {
        ui.Text(role)
        extra
        aside
        if let message = on_message {
            ui.Button("Message") {
                on press: message
            }
        }
        ui.Button("More") {
            on press: set(open, not open)
        }
    }
}
view people(n: Note) {
    state last: Text = ""
    stack {
        person(n.title)
        person(n.title, role: "Lead", online: true) {
            ui.Badge("New")
            aside: {
                ui.Text("Beside")
            }
            on message: set(last, n.title)
        }
    }
}"#,
        ));
        validate(&c.wasm);
    }

    phase an_event_answers_a_function_parameter_with_its_value;
    {
        let c = compile_body(&with_note(
            r#"view picker(choices: List<Text>, on_pick: (Text) -> Action) {
    stack {
        for x in choices {
            ui.Button(x) {
                on press: on_pick(x)
            }
        }
    }
}
view page_of(n: Note) {
    state chosen: Text = ""
    picker(["a", "b"]) {
        on pick(x): set(chosen, x)
    }
}"#,
        ));
        validate(&c.wasm);
    }

    phase what_a_call_cannot_give_is_refused;
    {
        let (code, message) = one_error(&with_note(
            r#"view plain(title: Text) {
    ui.Text(title)
}
view page_of(n: Note) {
    plain(n.title) {
        ui.Text("child")
    }
}"#,
        ));
        assert_eq!(code, "E063");
        assert!(message.contains("View"), "{message}");
        let got = errors(&with_note(
            r"view card_of(title: Text, extra: View) {
    ui.Text(title)
    extra
}
view page_of(n: Note) {
    card_of(n.title) {
        on vanish: open(n)
    }
}",
        ));
        assert!(
            got.iter().any(|(_, m)| m.contains("vanish") || m.contains("no parameter")),
            "an event no parameter answers is refused: {got:#?}"
        );
        let got = errors("fn f(x: Int = 2) -> Int {\n    x\n}");
        assert!(
            got.iter().any(|(code, _)| code == "E028"),
            "a function's parameters still take no default: {got:#?}"
        );
    }
}

const REACTED: &str = r"type Post {
    text: Text
    reactions: Map<Set<Actor>>
}
";

const CALCULATIONS: &str = r#"type Calculation {
    expr: Text
    result: Text
}

type Calculator {
    past: Collection<Calculation>
}

view calculation(x: Calculation) {
    ui.Text("{x.expr} = {x.result}")
}

default view calculation for Calculation

fn expression_of(x: Calculation) -> Text {
    x.expr
}

action rerun(x: Calculation, expr: Text) {
    x.expr = expr
}

/// The calculations as a list beside an editor; `HANDLER` is what activating one does.
view home(c: Calculator) {
    state expr: Text = ""
    state fresh: Bool = true
    ui.TextInput(bind: expr, placeholder: "Expression")
    ui.VirtualList {
        on activate(x: Calculation): HANDLER
        for x in c.past {
            ui.ListItem(item: x) {
                ui.Text(x.expr)
            }
        }
    }
}
"#;

fn calculations(handler: &str) -> String {
    CALCULATIONS.replace("HANDLER", handler)
}

tilo_test_env::scenario! {
    /// What an event carries may set the view's own state through a pure expression: the editor
    /// loads the calculation a person activates. Anything that changes a resource still takes the
    /// value as it is, and nothing but a function computes the state's new value.
    fn an_event_value_sets_view_state_through_a_pure_expression;

    phase a_field_or_a_function_of_the_value_sets_the_views_state;
    {
        for handler in [
            "set(expr, x.expr).then(set(fresh, false))",
            "set(expr, expression_of(x))",
            "set(fresh, x.result == \"\").then(open(x))",
        ] {
            let got = errors(&calculations(handler));
            assert!(got.is_empty(), "`{handler}` is accepted: {got:#?}");
        }
    }

    phase an_action_on_a_resource_still_takes_the_value_as_it_is;
    {
        let (code, message) = one_error(&calculations("rerun(x, x.expr)"));
        assert_eq!(code, "E063");
        assert!(message.contains("set(state, f(x))"), "{message}");
        let (code, _) = one_error(&calculations("set(expr, \"\").then(rerun(x, x.expr))"));
        assert_eq!(code, "E063", "a step of a chain that changes a resource");
    }

    phase the_state_set_is_the_views_own;
    {
        let (code, _) = one_error(&calculations("set(c, x.expr)"));
        assert_eq!(code, "E063", "the view's parameter is no state of its own");
    }

    phase the_new_value_is_computed_without_acting;
    {
        let got = errors(&calculations("set(expr, rerun(x, \"\"))"));
        assert!(
            got.iter().any(|(code, _)| code == "E043"),
            "the value is a function's, which uses no action: {got:#?}"
        );
    }
}

const CLOCKS: &str = r#"type Stopwatch {
    elapsed: Int = 0
    running: Bool = false
    laps: List<Int>
}

type Clocks {
    stopwatch: Stopwatch
    NEXT
}

root Clocks

view clocks(c: Clocks) {
    ui.Text("{c.stopwatch.elapsed}")
}

default view clocks for Clocks
"#;

fn clocks(next: &str, more: &str) -> String {
    format!("{}\n{more}", CLOCKS.replace("NEXT", next))
}

tilo_test_env::scenario! {
    /// A record field a resource is made without starts as a record of its own, from its fields'
    /// defaults: the checker refuses one that cannot start that way where the field is left out,
    /// never a read that finds nothing.
    fn a_record_field_left_out_starts_from_its_defaults;

    phase a_record_of_defaults_is_held_by_a_root;
    {
        let got = errors(&clocks("", ""));
        assert!(got.is_empty(), "{got:#?}");
    }

    phase a_held_record_with_a_field_without_a_default_is_refused;
    {
        let got = errors(&clocks("alarm: Alarm", "type Alarm {\n    rings: Actor\n}"));
        assert!(
            got.iter().any(|(code, m)| code == "E019"
                && m.contains("without `alarm`")
                && m.contains("the `rings` of a `Alarm` has no default")),
            "{got:#?}"
        );
    }

    phase a_held_record_that_holds_itself_again_is_refused;
    {
        let got = errors(&clocks("alarm: Alarm", "type Alarm {\n    clocks: Clocks\n}"));
        assert!(
            got.iter().any(|(code, m)| code == "E019" && m.contains("would never end")),
            "{got:#?}"
        );
    }

    phase a_record_given_where_it_is_made_is_a_reference_as_before;
    {
        let more = "type Alarm {\n    rings: Actor\n}\n\ntype Ring {\n    alarm: Alarm\n}\n\ntype Rings {\n    rings: Collection<Ring>\n}\n\naction ring(r: Rings, a: Alarm) {\n    r.rings.insert(Ring(alarm: a))\n}\n\naction loose(r: Rings) {\n    r.rings.insert(Ring())\n}";
        let got = errors(&clocks("", more));
        assert_eq!(got.len(), 1, "only the making that leaves it out: {got:#?}");
        assert!(got[0].1.contains("a `Ring` made without `alarm`"), "{got:#?}");
    }
}

tilo_test_env::scenario! {
    /// A `template view` expands where it is called with the names of the package that declares
    /// it: a call it makes to one of that package's views reaches the view, never the private row
    /// view a loop inside that view made under the same name.
    fn a_templates_call_reaches_the_view_not_the_row_of_its_name;

    phase the_rows_of_a_view_do_not_answer_for_it_in_another_package;
    {
        // A view with a loop inside it: the checker makes `peek_card~row0` of the loop body, and
        // the row is a private part of the view it is written in.
        let shell = crate::common::compile(
            "app t.a \"A\"\n\
             type Peek {\n    hints: Collection<Text>\n}\n\
             /// The keys a held key reaches\n\
             view peek_card(p: Peek) {\n    stack {\n        for h in p.hints {\n            text(h)\n        }\n    }\n}\n\
             template view around(p: Peek) {\n    peek_card(p)\n}\n",
        );
        // Another package calls the template: the body is rewritten to `a.peek_card(p)` with the
        // declaring package's mark, and that name is the declared view, not the row.
        let caller = crate::common::compile_with(
            "app t.b \"B\"\n\
             use t.a as a\n\
             view mine(p: a.Peek) {\n    a.around(p)\n}\n",
            &[shell.meta],
        )
        .unwrap_or_else(|d| panic!("the caller compiles: {d:#?}"));
        validate(&caller.wasm);
    }
}
