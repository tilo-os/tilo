//! The window's tests: what a person and an app see of one window.

use super::*;
use crate::adapter::{SLOT, kind};
use tilo_render_protocol::patch::local;

#[test]
fn a_splice_is_where_the_texts_differ() {
    assert_eq!(
        splice_between("Q3 plan", "Q3 plans"),
        (7, 0, "s".to_owned())
    );
    assert_eq!(splice_between("Q3 plans", "Q3"), (2, 6, String::new()));
    assert_eq!(splice_between("abc", "aXc"), (1, 1, "X".to_owned()));
    assert_eq!(splice_between("", "hi"), (0, 0, "hi".to_owned()));
    assert_eq!(splice_between("aa", "aaa"), (2, 0, "a".to_owned()));
    assert_eq!(splice_between("né", "nè"), (1, 2, "è".to_owned()));
    for (before, after) in [("naïve", "naive"), ("日本", "日本語"), ("x", "")] {
        let (at, delete, insert) = splice_between(before, after);
        let mut text = before.to_owned();
        text.replace_range(at..at + delete, &insert);
        assert_eq!(text, after);
    }
}

/// An app and the window that holds its end.
fn window() -> (local::End, AppWindow) {
    let (app, end) = local::pair();
    (app, AppWindow::new(Box::new(end), Vec::new()))
}

fn send(app: &mut local::End, batch: &[ToUi]) {
    app.send(wire::encode_batch(batch).expect("encodes"))
        .expect("sends");
}

fn sent(app: &mut local::End) -> Vec<ToApp> {
    let mut out = Vec::new();
    while let Some(batch) = app.recv().expect("receives") {
        out.extend(wire::decode_batch::<ToApp>(&batch).expect("a batch"));
    }
    out
}

/// The actions a press asked the page to run.
fn fired(app: &mut local::End) -> Vec<ToApp> {
    sent(app)
        .into_iter()
        .filter(|message| matches!(message, ToApp::Fire { .. }))
        .collect()
}

/// The note the page shows.
fn doc() -> ResourceRef {
    ResourceRef {
        id: "doc".to_owned(),
        ty: "notes.Note".to_owned(),
    }
}

/// A page with one text field bound to a note's title.
fn titled_page() -> Vec<ToUi> {
    let mut input = UiNode::empty("ui.TextInput", "title");
    input.binding = Some(crate::Binding {
        field: Some(("doc/notes:a".to_owned(), "title".to_owned())),
        kind: Some("notes.Note".to_owned()),
        name: "Title".to_owned(),
    });
    let mut page = UiNode::empty("stack", "page");
    page.children.push(input);
    vec![
        ToUi::Open {
            lane: Lane::Root,
            target: doc(),
            tree: page,
            front: None,
        },
        ToUi::Bound {
            lane: Lane::Root,
            key: "title".to_owned(),
            value: Value::text("Q3"),
            acked: 0,
        },
    ]
}

/// The spaces the window told the app, as (lane, width, height) in rem.
fn spaces(app: &mut local::End) -> Vec<(Lane, f32, f32)> {
    sent(app)
        .into_iter()
        .filter_map(|message| match message {
            ToApp::Space {
                lane,
                width,
                height,
            } => Some((lane, width, height)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_view_is_told_the_space_layout_gave_it_when_it_changes_by_more_than_a_step() {
    let (mut app, mut window) = window();
    send(&mut app, &titled_page());
    window.receive();
    let mut harness =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (390, 844));
    let flush = |harness: &mut ui::headless::FrameHarness| {
        harness
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AppWindow>())
            .expect("the window")
            .flush();
    };
    flush(&mut harness);
    let rem = f64::from(ui::widget::arrange::rem_of(harness.theme()));
    let told = spaces(&mut app);
    let [(Lane::Root, width, height)] = told[..] else {
        panic!("the root view is told its space once: {told:?}");
    };
    assert!(
        (f64::from(width) * rem - 390.0).abs() < 1.0 && f64::from(height) * rem <= 844.0,
        "the window's width, in rem: {width} × {height} at {rem} px"
    );
    // A pixel more moves nothing a view could read: nothing is said.
    harness.resize((391, 844)).expect("lays out");
    flush(&mut harness);
    assert_eq!(spaces(&mut app), []);
    // A desktop window is a new space.
    harness.resize((1280, 800)).expect("lays out");
    flush(&mut harness);
    let told = spaces(&mut app);
    assert!(
        matches!(told[..], [(Lane::Root, width, _)] if (f64::from(width) * rem - 1280.0).abs() < 1.0),
        "{told:?}"
    );
}

#[test]
fn typing_is_sent_as_splices_and_what_the_app_sent_before_it_saw_them_is_not_shown() {
    let (mut app, mut window) = window();
    send(&mut app, &titled_page());
    window.receive();
    assert_eq!(window.bound_value("title"), Some(Value::text("Q3")));
    // The person types: the field shows it at once (it is the toolkit's), and the app hears
    // of it as a splice of what the field showed.
    window.act(UiEvent::Edit("title".to_owned(), "Q3 plan".to_owned()));
    window.flush();
    assert_eq!(
        sent(&mut app),
        [ToApp::Splice {
            lane: Lane::Root,
            target: doc(),
            key: "title".to_owned(),
            seq: 1,
            at: 2,
            delete: 0,
            insert: " plan".to_owned(),
        }]
    );
    // What the app sent before it took the splice does not take the typing back.
    send(
        &mut app,
        &[ToUi::Bound {
            lane: Lane::Root,
            key: "title".to_owned(),
            value: Value::text("Q3"),
            acked: 0,
        }],
    );
    window.receive();
    assert_eq!(
        window.bound_value("title"),
        Some(Value::text("Q3 plan")),
        "the field keeps what the person typed"
    );
    window.act(UiEvent::Edit("title".to_owned(), "Q3 plans".to_owned()));
    window.flush();
    assert!(
        matches!(&sent(&mut app)[..], [ToApp::Splice { seq: 2, at: 7, delete: 0, insert, .. }] if insert == "s"),
        "the next splice is against what the person typed"
    );
    // What it sent once it took them is shown.
    send(
        &mut app,
        &[ToUi::Bound {
            lane: Lane::Root,
            key: "title".to_owned(),
            value: Value::text("Q3 plans"),
            acked: 2,
        }],
    );
    window.receive();
    assert_eq!(window.bound_value("title"), Some(Value::text("Q3 plans")));
}

#[test]
fn typing_after_a_press_goes_to_the_field_as_the_press_left_it_once_the_app_ran_it() {
    let (mut app, mut window) = window();
    send(&mut app, &titled_page());
    window.receive();
    // A press, and typing at once into the field focused: the typing waits for the press.
    window.act(UiEvent::Press("new".to_owned()));
    window.act(UiEvent::Edit("title".to_owned(), "Q3 x".to_owned()));
    window.flush();
    assert!(
        matches!(&sent(&mut app)[..], [ToApp::Fire { key, .. }] if key == "new"),
        "the press goes at once, the typing waits"
    );
    window.act(UiEvent::Edit("title".to_owned(), "Q3 xy".to_owned()));
    window.flush();
    assert!(sent(&mut app).is_empty(), "still waiting for the press");
    // The press opened another note into the field, and the app says it ran it: what was
    // typed goes to that note's title, at its end.
    let mut opened = titled_page();
    opened[1] = ToUi::Bound {
        lane: Lane::Root,
        key: "title".to_owned(),
        value: Value::text("Untitled"),
        acked: 0,
    };
    opened.push(ToUi::Took { fired: 1 });
    send(&mut app, &opened);
    window.receive();
    window.flush();
    assert_eq!(
        sent(&mut app),
        [ToApp::Splice {
            lane: Lane::Root,
            target: doc(),
            key: "title".to_owned(),
            seq: 1,
            at: 8,
            delete: 0,
            insert: " xy".to_owned(),
        }]
    );
    assert_eq!(
        window.bound_value("title"),
        Some(Value::text("Untitled xy"))
    );
    // From here on typing goes as it is typed.
    window.act(UiEvent::Edit("title".to_owned(), "Untitled xyz".to_owned()));
    window.flush();
    assert!(
        matches!(&sent(&mut app)[..], [ToApp::Splice { seq: 2, at: 11, insert, .. }] if insert == "z")
    );
}

/// The note `id`.
fn note(id: &str) -> ResourceRef {
    ResourceRef {
        id: id.to_owned(),
        ty: "notes.Note".to_owned(),
    }
}

/// The note `id` opened beside the page, its title showing `title`.
fn opened_note(id: &str, title: &str) -> Vec<ToUi> {
    let mut input = UiNode::empty("ui.TextInput", "title");
    input.binding = Some(crate::Binding {
        field: Some((format!("{id}/notes:a"), "title".to_owned())),
        kind: Some("notes.Note".to_owned()),
        name: "Title".to_owned(),
    });
    let mut page = UiNode::empty("stack", "note");
    page.children.push(input);
    vec![
        ToUi::Open {
            lane: Lane::Detail,
            target: note(id),
            tree: page,
            front: Some(wire::Front::Input),
        },
        ToUi::Bound {
            lane: Lane::Detail,
            key: "title".to_owned(),
            value: Value::text(title),
            acked: 0,
        },
    ]
}

#[test]
fn typing_into_a_field_put_the_keyboard_in_while_a_press_runs_stays_with_its_note() {
    let (mut app, mut window) = window();
    send(&mut app, &titled_page());
    send(&mut app, &opened_note("a", "Alpha"));
    window.receive();
    let title = format!("{DETAIL}title");
    // The person presses New, clicks into note a's title and types there before the app
    // ran the press.
    window.act(UiEvent::Press("new".to_owned()));
    window.flush();
    assert!(
        matches!(&sent(&mut app)[..], [ToApp::Fire { key, .. }] if key == "new"),
        "the press goes at once"
    );
    window.pointed_after = Some(window.fired);
    window.act(UiEvent::Edit(title.clone(), "Alpha!".to_owned()));
    window.flush();
    assert!(sent(&mut app).is_empty(), "the typing waits for the press");
    // The press opened note b, and the app says it ran it.
    let mut opened = opened_note("b", "Untitled");
    opened.push(ToUi::Took { fired: 1 });
    send(&mut app, &opened);
    window.receive();
    window.flush();
    assert_eq!(
        sent(&mut app),
        [ToApp::Splice {
            lane: Lane::Detail,
            target: note("a"),
            key: "title".to_owned(),
            seq: 0,
            at: 5,
            delete: 0,
            insert: "!".to_owned(),
        }],
        "what was typed into note a's title goes to note a"
    );
    assert_eq!(
        window.bound_value(&title),
        Some(Value::text("Untitled")),
        "note b's title shows what it holds, none of note a's typing"
    );
}

#[test]
fn typing_after_a_press_in_the_field_that_still_had_the_keyboard_follows_into_what_it_opens() {
    let (mut app, mut window) = window();
    send(&mut app, &titled_page());
    send(&mut app, &opened_note("a", "Alpha"));
    window.receive();
    let title = format!("{DETAIL}title");
    // The person presses New (in the window's bar, which leaves the keyboard in note a's
    // title) and types on at once, before the app ran the press.
    window.act(UiEvent::Press("new".to_owned()));
    window.act(UiEvent::Edit(title.clone(), "AlphaEcho".to_owned()));
    window.flush();
    assert!(
        matches!(&sent(&mut app)[..], [ToApp::Fire { key, .. }] if key == "new"),
        "the typing waits for the press"
    );
    let mut opened = opened_note("b", "Untitled");
    opened.push(ToUi::Took { fired: 1 });
    send(&mut app, &opened);
    window.receive();
    window.flush();
    assert_eq!(
        sent(&mut app),
        [ToApp::Splice {
            lane: Lane::Detail,
            target: note("b"),
            key: "title".to_owned(),
            seq: 1,
            at: 8,
            delete: 0,
            insert: "Echo".to_owned(),
        }],
        "only what was typed after the press goes to note b's title, where the press left \
         the keyboard; note a keeps what it held"
    );
    assert_eq!(
        window.bound_value(&title),
        Some(Value::text("UntitledEcho"))
    );
}

#[test]
fn typing_between_what_a_press_opened_and_its_took_follows_into_the_opened_field() {
    let (mut app, mut window) = window();
    send(&mut app, &titled_page());
    send(&mut app, &opened_note("a", "Alpha"));
    window.receive();
    let title = format!("{DETAIL}title");
    window.act(UiEvent::Press("new".to_owned()));
    window.flush();
    let _ = sent(&mut app);
    // The note the press opened arrives in one batch, the app's Took in the next; the
    // person types in between, in the field that had the keyboard, which shows note b now.
    send(&mut app, &opened_note("b", ""));
    window.receive();
    window.act(UiEvent::Edit(title, "Echo".to_owned()));
    window.flush();
    assert!(sent(&mut app).is_empty(), "the typing waits for the press");
    send(&mut app, &[ToUi::Took { fired: 1 }]);
    window.receive();
    window.flush();
    assert!(
        matches!(&sent(&mut app)[..], [ToApp::Splice { target, insert, .. }] if *target == note("b") && insert == "Echo"),
        "the typing goes to the opened note's field"
    );
}

#[test]
fn a_batch_that_breaks_the_protocol_shows_the_error_state_and_nothing_else() {
    let (mut app, mut window) = window();
    let unknown = vec![ToUi::Open {
        lane: Lane::Root,
        target: ResourceRef {
            id: "doc".to_owned(),
            ty: "app.Root".to_owned(),
        },
        tree: UiNode::empty("ui.NoSuchComponent", "root"),
        front: None,
    }];
    app.send(wire::encode_batch(&unknown).expect("encodes"))
        .expect("sends");
    assert!(window.receive());
    assert!(window.failed().is_some());
    assert!(window.composed().is_none(), "nothing of it was built");
    app.send(b"scribbles".to_vec()).expect("sends");
    assert!(window.receive(), "a scribble is refused, not a panic");
}

/// What the app published for the window: a resource with one action, as the catalogue
/// carries it.
fn catalogue_of(label: &str, shortcut: Option<&str>) -> Vec<u8> {
    use tilo_actions::{ActionSpec, OperationKind, ResourceSpec};
    let mut spec = ActionSpec::operation(
        "new",
        label,
        OperationKind::Effect,
        "test.create",
        &serde_json::json!({}),
    )
    .unwrap();
    spec.type_mut().state.shortcut = shortcut.map(str::to_owned);
    let resource = ResourceSpec::new("tasks:1", "test.tasks", "Tasks").with_action(spec);
    serde_json::to_vec(&tilo_actions::door::survey(&[resource])).expect("encodes")
}

/// The window offers the actions its app publishes, and a catalogue of labels that moved
/// replaces the action it holds where it sits, with the key the app gives it now.
#[test]
fn a_published_catalogue_is_what_the_windows_palette_offers() {
    let (mut app, mut window) = window();
    send(
        &mut app,
        &[ToUi::Actions(wire::ActionCatalogue::All(catalogue_of(
            "New task",
            Some("Ctrl+n"),
        )))],
    );
    window.receive();
    assert_eq!(
        window.actions().first().map(|action| (
            action.action_label.as_str(),
            action.state.shortcut.as_deref()
        )),
        Some(("New task", Some("Ctrl+n")))
    );
    send(
        &mut app,
        &[ToUi::Actions(wire::ActionCatalogue::Relabeled(
            catalogue_of("Create task", Some("Ctrl+enter")),
        ))],
    );
    window.receive();
    assert_eq!(
        window.actions().first().map(|action| (
            action.action_label.as_str(),
            action.state.shortcut.as_deref()
        )),
        Some(("Create task", Some("Ctrl+enter"))),
        "one action, relabeled in place"
    );
}

/// The window declares each published action as the app declared it — what it takes, and
/// whether it can run now — because that is what its palette lists by.
#[test]
fn the_windows_catalogue_keeps_what_an_action_takes_and_whether_it_can_run() {
    use tilo_actions::{ActionParam, ActionSpec, ExecutionLocus, ParamType, Pick, ResourceSpec};
    let spec = ActionSpec::typed(
        "remove",
        "Remove",
        "Remove",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("task", ParamType::Object, "task: Task").picking(
                Pick::Resource {
                    kind: "test.tasks.Task".to_owned(),
                },
            ),
        ],
        |_args| tilo_actions::EncodedAction {
            type_id: "test.create".to_owned(),
            payload: Vec::new(),
        },
    )
    .unavailable("nothing_selected", "Choose a task first");
    let resource = ResourceSpec::new("tasks:1", "test.tasks", "Tasks").with_action(spec);
    let published = serde_json::to_vec(&tilo_actions::door::survey(&[resource])).expect("encodes");

    let (mut app, mut window) = window();
    send(
        &mut app,
        &[ToUi::Actions(wire::ActionCatalogue::All(published))],
    );
    window.receive();
    let catalogue = window.catalogue();
    let action = &catalogue
        .first()
        .and_then(|resource| resource.actions.first())
        .expect("the app's action is declared for the window");
    assert!(
        !action.takes_a_row_answer(),
        "a task is picked where tasks are, not typed into a row"
    );
    assert!(
        !action.contract.availability.is_available(),
        "and the app says it cannot run now"
    );
}

/// A view whose header offers `Archive` as a button declaring `key`, as `std.header` builds
/// it for an action with that shortcut: the `key` prop and the button's `press`.
fn keyed_menu(key: &str) -> Vec<ToUi> {
    let mut button = UiNode::empty("ui.Button", "archive");
    button
        .props
        .insert("label".to_owned(), Value::text("Archive"));
    button.props.insert("key".to_owned(), Value::text(key));
    button
        .events
        .insert("press".to_owned(), wire::Event::default());
    let mut shell = UiNode::empty("ui.NavShell", "shell");
    let mut actions = UiNode::empty("slot", "actions");
    actions
        .props
        .insert("name".to_owned(), Value::text("actions"));
    actions.children.push(button);
    shell.children.push(actions);
    vec![ToUi::Open {
        lane: Lane::Root,
        target: doc(),
        tree: shell,
        front: None,
    }]
}

#[test]
fn a_declared_shortcut_runs_the_action_through_its_button() {
    let (mut app, mut window) = window();
    send(&mut app, &keyed_menu("Ctrl+Enter"));
    window.receive();
    assert!(
        window.failed().is_none(),
        "the view was taken: {:?}",
        window.failed()
    );
    assert!(
        window.composed().is_some_and(|tree| shortcut_action_where(
            tree,
            None,
            &ui::platform::Shortcut::from("Ctrl+Enter")
        )
        .is_some()),
        "the composed shell carries the declared shortcut: {:#?}",
        window.composed()
    );
    let mut harness =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (900, 700));
    // The action's own `key:` names its shortcut: its modifiers with its key, and neither
    // half on its own, nor another modifier held with it.
    press(&mut harness, &["command"], "Enter");
    assert_eq!(
        fired(&mut app),
        [ToApp::Fire {
            lane: Lane::Root,
            key: "archive".to_owned(),
            event: "press".to_owned(),
            value: None,
        }],
        "the press ran the declared action, through its button"
    );
    for (modifiers, key) in [(&[][..], "Enter"), (&["command"][..], "E")] {
        press(&mut harness, modifiers, key);
        assert!(
            fired(&mut app).is_empty(),
            "{modifiers:?}+{key} is not the action's shortcut"
        );
    }
    press(&mut harness, &["command", "shift"], "Enter");
    assert!(
        fired(&mut app).is_empty(),
        "a modifier the shortcut does not name is another press"
    );
}

/// A plain key is a shortcut too, and a press with any modifier held is another.
#[test]
fn a_plain_declared_key_runs_it_alone() {
    let (mut app, mut window) = window();
    send(&mut app, &keyed_menu("e"));
    window.receive();
    let mut harness =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (900, 700));
    harness.set_modifiers(&["command".to_owned()]);
    harness.key("e");
    harness.set_modifiers(&[]);
    {
        let window = harness
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AppWindow>())
            .expect("the window");
        window.flush();
    }
    assert!(
        fired(&mut app).is_empty(),
        "the command key is not the plain key"
    );
    harness.key("e");
    {
        let window = harness
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AppWindow>())
            .expect("the window");
        window.flush();
    }
    let fired: Vec<ToApp> = sent(&mut app)
        .into_iter()
        .filter(|message| matches!(message, ToApp::Fire { .. }))
        .collect();
    assert_eq!(fired.len(), 1, "the plain key ran it: {fired:?}");
    // A key no offered action declares is not the window's: typing and chords pass through.
    harness.key("q");
    {
        let window = harness
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AppWindow>())
            .expect("the window");
        window.flush();
    }
    let fired: Vec<ToApp> = sent(&mut app)
        .into_iter()
        .filter(|message| matches!(message, ToApp::Fire { .. }))
        .collect();
    assert!(fired.is_empty(), "`q` declares nothing: {fired:?}");
}

/// The window's primary action is where a person comes to add a list's next item: run from the
/// bar with nothing typed, it puts the keyboard in the list's entry, which declares the action's
/// key, rather than making an item nobody typed — whether it is chosen from the window's
/// catalogue or pressed where it stands.
#[test]
fn the_primary_action_puts_the_keyboard_in_the_entry_that_declares_its_key() {
    use wire::Event;
    let mut page = bound_page(
        "ui.TextInput",
        &[
            ("key", Value::text("Ctrl+n")),
            ("entry", Value::Bool(true)),
            ("placeholder", Value::text("New task")),
        ],
        Value::text(""),
    );
    // The window's bar, with the primary action's button in it.
    let mut button = UiNode::empty("ui.Button", "bar");
    button
        .props
        .insert("label".to_owned(), Value::text("New task"));
    button.props.insert("key".to_owned(), Value::text("Ctrl+n"));
    button
        .props
        .insert("role".to_owned(), Value::text("primary"));
    button.events.insert("press".to_owned(), Event::default());
    if let Some(ToUi::Open { tree, .. }) = page.first_mut() {
        tree.children.insert(0, button);
    }
    page.push(ToUi::Actions(wire::ActionCatalogue::All(catalogue_of(
        "New task",
        Some("Ctrl+n"),
    ))));
    let (mut app, mut harness) = drawn(&page);
    let made = |app: &mut _| -> Vec<ToApp> {
        told(app)
            .into_iter()
            .filter(|message| matches!(message, ToApp::Invoke { .. } | ToApp::Fire { .. }))
            .collect()
    };
    let address = tilo_actions::ActionAddress::new("tasks:1", "new");
    drawn_window(&mut harness).invoke_action(address, None);
    let ran = made(&mut app);
    assert!(ran.is_empty(), "nothing is made until it is typed: {ran:?}");
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_field().as_deref(),
        Some("field"),
        "the keyboard is in the entry, where the item is typed"
    );
    // Pressed where it stands, the bar's button leads the person there too.
    drawn_window(&mut harness).act(UiEvent::Press("bar".to_owned()));
    drawn_window(&mut harness).flush();
    let ran = made(&mut app);
    assert!(
        ran.is_empty(),
        "the press makes no item nobody typed: {ran:?}"
    );
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_field().as_deref(),
        Some("field"),
        "and it leaves the keyboard in the entry"
    );
}

/// Press `key` with `modifiers` held, as a person does, and let the window send what it did.
fn press(harness: &mut ui::headless::FrameHarness, modifiers: &[&str], key: &str) {
    let modifiers: Vec<String> = modifiers.iter().map(|name| (*name).to_owned()).collect();
    harness.set_modifiers(&modifiers);
    harness.key(key);
    harness.set_modifiers(&[]);
    drawn_window(harness).flush();
}

/// What the control called `name` shows, as assistive technology reads it.
fn shown_value(harness: &ui::headless::FrameHarness, name: &str) -> Option<String> {
    let named = node_named(harness, name);
    harness
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == named)
        .and_then(|(_, node)| node.value().map(str::to_owned))
}

/// The entry a list's next item is typed in declares the primary action's key: pressed there,
/// the key commits what was typed through the entry's own submit, rather than making an empty
/// item.
#[test]
fn the_primary_actions_key_in_the_entry_commits_what_was_typed() {
    use wire::Event;
    let mut page = bound_page(
        "ui.TextInput",
        &[
            ("key", Value::text("Ctrl+n")),
            ("entry", Value::Bool(true)),
            ("placeholder", Value::text("New task")),
        ],
        Value::text(""),
    );
    if let Some(ToUi::Open { tree, .. }) = page.first_mut() {
        tree.children[0]
            .events
            .insert("submit".to_owned(), Event::default());
        // The bar's own button for the same action.
        let mut button = UiNode::empty("ui.Button", "bar");
        button
            .props
            .insert("label".to_owned(), Value::text("New task"));
        button.props.insert("key".to_owned(), Value::text("Ctrl+n"));
        button.events.insert("press".to_owned(), Event::default());
        tree.children.insert(0, button);
    }
    page.push(ToUi::Actions(wire::ActionCatalogue::All(catalogue_of(
        "New task",
        Some("Ctrl+n"),
    ))));
    let (mut app, mut harness) = drawn(&page);
    drawn_window(&mut harness)
        .invoke_action(tilo_actions::ActionAddress::new("tasks:1", "new"), None);
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_field().as_deref(),
        Some("field"),
        "the primary action put the keyboard in the entry"
    );
    harness.type_text("Buy milk");
    press(&mut harness, &["command"], "n");
    let told = told(&mut app);
    let fires: Vec<ToApp> = told
        .iter()
        .filter(|message| matches!(message, ToApp::Fire { .. }))
        .cloned()
        .collect();
    assert_eq!(
        fires,
        [ToApp::Fire {
            lane: Lane::Root,
            key: "field".to_owned(),
            event: "submit".to_owned(),
            value: Some(Value::text("Buy milk")),
        }],
        "the key committed the item typed in the entry, as its own Enter does: {told:?}"
    );
}

/// Two items of a list each declare the key that completes them: the press completes the item
/// the keyboard is on, not the first one the window shows.
#[test]
fn a_press_runs_the_action_of_the_item_the_keyboard_is_on() {
    use wire::Event;
    let mut page = UiNode::empty("stack", "page");
    let mut page_open = Vec::new();
    for (key, name) in [("a", "First"), ("b", "Second")] {
        let mut item = UiNode::empty("stack", key);
        let mut complete = UiNode::empty("ui.Button", &format!("{key}#complete"));
        complete
            .props
            .insert("label".to_owned(), Value::text("Complete"));
        complete
            .props
            .insert("key".to_owned(), Value::text("Ctrl+Enter"));
        complete.events.insert("press".to_owned(), Event::default());
        let mut title = UiNode::empty("ui.TextInput", &format!("{key}#title"));
        title.binding = Some(crate::Binding {
            field: Some((format!("doc/tasks:{key}"), "title".to_owned())),
            kind: Some("tasks.Task".to_owned()),
            name: name.to_owned(),
        });
        title
            .props
            .insert("placeholder".to_owned(), Value::text(name));
        item.children.push(complete);
        item.children.push(title);
        page.children.push(item);
        page_open.push(ToUi::Bound {
            lane: Lane::Root,
            key: format!("{key}#title"),
            value: Value::text(format!("{name} task")),
            acked: 0,
        });
    }
    let mut open = vec![ToUi::Open {
        lane: Lane::Root,
        target: doc(),
        tree: page,
        front: None,
    }];
    open.extend(page_open);
    let (mut app, mut harness) = drawn(&open);
    harness
        .click_named("b#title")
        .expect("the second item's title is drawn");
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_control().as_deref(),
        Some("b#title"),
        "the keyboard is in the second item's title"
    );
    press(&mut harness, &["command"], "Enter");
    assert_eq!(
        fired(&mut app),
        [ToApp::Fire {
            lane: Lane::Root,
            key: "b#complete".to_owned(),
            event: "press".to_owned(),
            value: None,
        }],
        "the press completed the item the keyboard is on, not the first one"
    );
}

/// A field's own keys are the field's: Delete edits the text the keyboard is in, and the action
/// that declares Delete runs only where no field has the keyboard.
#[test]
fn a_fields_own_keys_edit_its_text_before_any_action_declares_them() {
    use wire::Event;
    let mut page = bound_page(
        "ui.TextInput",
        &[("placeholder", Value::text("Title"))],
        Value::text("Q3 plan"),
    );
    if let Some(ToUi::Open { tree, .. }) = page.first_mut() {
        let mut erase = UiNode::empty("ui.Button", "erase");
        erase
            .props
            .insert("label".to_owned(), Value::text("Delete"));
        erase.props.insert("key".to_owned(), Value::text("Delete"));
        erase.events.insert("press".to_owned(), Event::default());
        tree.children.push(erase);
    }
    let (mut app, mut harness) = drawn(&page);
    // With no field to type in, the key is the action's.
    harness.key("Delete");
    drawn_window(&mut harness).flush();
    assert_eq!(
        fired(&mut app),
        [ToApp::Fire {
            lane: Lane::Root,
            key: "erase".to_owned(),
            event: "press".to_owned(),
            value: None,
        }],
        "the declared action runs where nothing takes typing"
    );
    // In the field, the same key edits the text it holds.
    harness.click_named("field").expect("the title is drawn");
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_control().as_deref(),
        Some("field"),
        "the click put the keyboard in the title"
    );
    harness.key("Home");
    harness.key("Delete");
    harness.relayout();
    assert_eq!(
        shown_value(&harness, "Field").as_deref(),
        Some("3 plan"),
        "Delete took the character out of the text the field holds"
    );
    assert!(
        fired(&mut app).is_empty(),
        "the key in the field ran no action"
    );
}

/// The keys a field uses to select stay the field's: select all, then typing replaces the text.
#[test]
fn a_fields_select_all_stays_in_the_field() {
    let (mut app, mut harness) = drawn(&bound_page(
        "ui.TextInput",
        &[("placeholder", Value::text("Title"))],
        Value::text("Q3 plan"),
    ));
    harness.click_named("field").expect("the title is drawn");
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_control().as_deref(),
        Some("field"),
        "the click put the keyboard in the title"
    );
    press(&mut harness, &["command"], "a");
    harness.type_text("Q4 plan");
    harness.relayout();
    assert_eq!(
        shown_value(&harness, "Field").as_deref(),
        Some("Q4 plan"),
        "select all, then typing replaced what the field held"
    );
    assert!(fired(&mut app).is_empty(), "the keys ran no action");
}

/// A page whose one described control `kind` is bound to a note's field and shows `value`,
/// with `props` as the view set them.
fn bound_page(kind: &str, props: &[(&str, Value)], value: Value) -> Vec<ToUi> {
    let mut control = UiNode::empty(kind, "field");
    control.binding = Some(crate::Binding {
        field: Some(("doc/notes:a".to_owned(), "field".to_owned())),
        kind: Some("notes.Note".to_owned()),
        name: "Field".to_owned(),
    });
    control
        .props
        .insert("bind".to_owned(), Value::text("value"));
    for (name, value) in props {
        control.props.insert((*name).to_owned(), value.clone());
    }
    let mut page = UiNode::empty("stack", "page");
    page.children.push(control);
    vec![
        ToUi::Open {
            lane: Lane::Root,
            target: doc(),
            tree: page,
            front: None,
        },
        ToUi::Bound {
            lane: Lane::Root,
            key: "field".to_owned(),
            value,
            acked: 0,
        },
    ]
}

/// A window showing `page`, drawn once.
fn drawn(page: &[ToUi]) -> (local::End, ui::headless::FrameHarness) {
    let (mut app, mut window) = window();
    send(&mut app, page);
    window.receive();
    let mut harness =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (1280, 800));
    harness.relayout();
    (app, harness)
}

/// The window the harness draws.
fn drawn_window(harness: &mut ui::headless::FrameHarness) -> &mut AppWindow {
    harness
        .root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<AppWindow>())
        .expect("the window")
}

/// What the window told the app, apart from the space it is drawn in.
fn told(app: &mut local::End) -> Vec<ToApp> {
    sent(app)
        .into_iter()
        .filter(|message| !matches!(message, ToApp::Space { .. }))
        .collect()
}

/// Run `test` with the clock at the day a capture shows (30 June 2026), so a day's fact reads
/// the same on every run rather than as "Overdue" once the day has gone by.
fn at_capture_day<T>(test: impl FnOnce() -> T) -> T {
    let at = ui::clock::capture_wall().expect("the capture day exists in the local zone");
    ui::time::testing::with_wall(at, test)
}

/// The accessibility node of the control named `name`.
fn node_named(harness: &ui::headless::FrameHarness, name: &str) -> ui::accesskit::NodeId {
    harness.a11y_node_named(name).unwrap_or_else(|| {
        panic!(
            "{name:?} is drawn; the tree names {:?}",
            harness
                .a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| node.label().map(str::to_owned))
                .collect::<Vec<_>>()
        )
    })
}

/// A value set elsewhere reaches the control showing it, whatever the control is.
#[test]
fn a_bound_control_shows_the_value_the_app_bound() {
    at_capture_day(|| {
        let rich = |text: &str| {
            Value::Rich(wire::rich::RichText {
                blocks: vec![wire::rich::Block {
                    kind: "paragraph".to_owned(),
                    text: text.to_owned(),
                    ..wire::rich::Block::default()
                }],
                ..wire::rich::RichText::default()
            })
        };
        let labelled = [
            ("label", Value::text("Due")),
            ("optional", Value::Bool(true)),
        ];
        let shown = |kind: &str, props: &[(&str, Value)], value: Value, name: &str| {
            let (_app, harness) = drawn(&bound_page(kind, props, value));
            let target = node_named(&harness, name);
            harness
                .a11y_nodes()
                .into_iter()
                .find(|(id, _)| *id == target)
                .map(|(_, node)| node)
                .expect("the node is in the tree")
        };
        // What the field is for, and the day the app bound as the day's own fact ("Due,
        // Today") — never as the "Add date" a field that holds no day offers.
        let today = ui::clock::today();
        let bound = ui::widget::DateField::spoken(Some("Due"), Some(today), None);
        assert_ne!(
            bound,
            ui::widget::DateField::spoken(Some("Due"), None, None),
            "a day the app bound reads as a day"
        );
        let day = shown(
            "ui.DateField",
            &labelled,
            Value::some(Value::text(today.format("%Y-%m-%d").to_string())),
            &bound,
        );
        assert_eq!(
            day.label(),
            Some(bound.as_str()),
            "the day the app bound is the day the field says"
        );
        let boxed = shown(
            "ui.Checkbox",
            &[("label", Value::text("Done"))],
            Value::Bool(true),
            "Done",
        );
        assert_eq!(
            boxed.toggled(),
            Some(ui::accesskit::Toggled::True),
            "the mark the app sent is shown"
        );
        let toggle = shown(
            "ui.Toggle",
            &[("label", Value::text("Sync"))],
            Value::Bool(true),
            "Sync",
        );
        assert_eq!(
            toggle.toggled(),
            Some(ui::accesskit::Toggled::True),
            "the side the app sent is shown"
        );
        let input = shown(
            "ui.TextInput",
            &[("placeholder", Value::text("Title"))],
            Value::text("Q3 plan"),
            "Field",
        );
        assert_eq!(
            input.value(),
            Some("Q3 plan"),
            "the text the app sent is shown"
        );
        let written = shown("ui.RichInput", &[], rich("Q3 plan"), "Field");
        assert_eq!(
            written.value(),
            Some("Q3 plan"),
            "the rich text the app sent is shown"
        );
    });
}

/// A day picked in the calendar reaches the app as the value the field now holds, not as a
/// splice of the text it reads as. A field its app gives no label reads as the day it holds.
#[test]
fn a_picked_day_is_sent_as_the_day_the_field_now_holds() {
    at_capture_day(|| {
        let day = |day: &str| {
            ui::widget::DateField::spoken(
                Some("Due"),
                chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok(),
                None,
            )
        };
        let (mut app, mut harness) = drawn(&bound_page(
            "ui.DateField",
            &[
                ("label", Value::text("Due")),
                ("optional", Value::Bool(true)),
            ],
            Value::some(Value::text("2026-06-30")),
        ));
        let field = node_named(&harness, &day("2026-06-30"));
        harness
            .perform_a11y_action(field, &ui::a11y::Action::Activate)
            .expect("the field opens its calendar");
        harness.relayout();
        let calendar = node_named(&harness, "Calendar");
        harness
            .perform_a11y_action(
                calendar,
                &ui::a11y::Action::SetValue("2026-07-01".to_owned()),
            )
            .expect("a day is picked");
        harness.relayout();
        drawn_window(&mut harness).flush();
        assert_eq!(
            told(&mut app),
            [ToApp::Edit {
                lane: Lane::Root,
                target: doc(),
                key: "field".to_owned(),
                text: "2026-07-01".to_owned(),
            }],
            "a picked day is the value the field now holds"
        );
        let shown = harness
            .a11y_nodes()
            .into_iter()
            .find_map(|(_, node)| node.label().map(str::to_owned))
            .filter(|label| *label == day("2026-07-01"));
        assert_eq!(
            shown,
            Some(day("2026-07-01")),
            "the picked day is the day the field shows"
        );
        // A day the app sets after the field is drawn shows in place.
        send(
            &mut app,
            &[ToUi::Bound {
                lane: Lane::Root,
                key: "field".to_owned(),
                value: Value::some(Value::text("2026-07-05")),
                acked: 1,
            }],
        );
        drawn_window(&mut harness).receive();
        harness.relayout();
        let shown = harness
            .a11y_nodes()
            .into_iter()
            .find_map(|(_, node)| node.label().map(str::to_owned))
            .filter(|label| *label == day("2026-07-05"));
        assert_eq!(
            shown,
            Some(day("2026-07-05")),
            "a day the app set later shows in place"
        );
    });
}

/// A mark or a turn reaches the app as the state the control now holds.
#[test]
fn a_mark_or_a_turn_is_sent_as_the_state_the_control_now_holds() {
    let (mut app, mut harness) = drawn(&bound_page(
        "ui.Checkbox",
        &[("label", Value::text("Done"))],
        Value::Bool(false),
    ));
    let boxed = node_named(&harness, "Done");
    harness
        .perform_a11y_action(boxed, &ui::a11y::Action::Activate)
        .expect("the box is chosen");
    harness.relayout();
    drawn_window(&mut harness).flush();
    assert_eq!(
        told(&mut app),
        [ToApp::Toggle {
            lane: Lane::Root,
            target: doc(),
            key: "field".to_owned(),
            checked: true,
        }],
        "a chose marks the box"
    );

    let (mut app, mut harness) = drawn(&bound_page(
        "ui.Toggle",
        &[("label", Value::text("Sync"))],
        Value::Bool(false),
    ));
    let toggle = node_named(&harness, "Sync");
    harness
        .perform_a11y_action(toggle, &ui::a11y::Action::Activate)
        .expect("the switch turns");
    harness.relayout();
    drawn_window(&mut harness).flush();
    assert_eq!(
        told(&mut app),
        [ToApp::Toggle {
            lane: Lane::Root,
            target: doc(),
            key: "field".to_owned(),
            checked: true,
        }],
        "a turn switches it on"
    );
}

/// A page whose overview is one list over the rows the app renders, as `std.list` builds it:
/// the list inside the page an app docks its composer or bar under.
fn list_page() -> Vec<ToUi> {
    list_page_keyed("shell")
}

/// [`list_page`], its shell under `key`: a new key is a shell the window builds anew.
fn list_page_keyed(key: &str) -> Vec<ToUi> {
    let mut list = UiNode::empty("ui.VirtualList", "list");
    list.props.insert("in_column".to_owned(), Value::Bool(true));
    let mut docked = UiNode::empty("ui.Docked", "docked");
    docked.children.push(list);
    let mut overview = UiNode::empty("slot", "overview");
    overview
        .props
        .insert("name".to_owned(), Value::text("overview"));
    overview.children.push(docked);
    let mut shell = UiNode::empty("ui.NavShell", key);
    shell.children.push(overview);
    vec![ToUi::Open {
        lane: Lane::Root,
        target: doc(),
        tree: shell,
        front: None,
    }]
}

/// A shell built anew opens on the screen a new window opens on; the window remembers the
/// column the person is looking at, and hands it to the shell it builds.
#[test]
fn a_window_keeps_the_column_in_front_when_its_shell_is_built_anew() {
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
    let front = |window: &mut AppWindow| window.shell_ref().map(NavShell::front_pane);
    send(&mut app, &list_page());
    window.receive();
    assert!(
        !window.shell_ref().expect("the shell").has_content(),
        "a list with nothing open lists for the whole window"
    );
    // The person taps a row: the window shows it in front of the list.
    send(&mut app, &opened_note("a", "Alpha"));
    window.receive();
    assert_eq!(front(&mut window), Some(NavPane::Content));
    // The app's views change key, so the shell is built anew.
    send(&mut app, &list_page_keyed("again"));
    window.receive();
    assert!(window.failed().is_none(), "{:?}", window.failed());
    assert_eq!(
        front(&mut window),
        Some(NavPane::Content),
        "the new shell shows the column the person is looking at, not the one a window opens on"
    );
    // Back closes the note: the list stands alone again, and the app saying a row opened brings
    // no column forward until one stands beside the list.
    send(
        &mut app,
        &[
            ToUi::Close,
            ToUi::Front {
                front: wire::Front::Input,
                shows: None,
            },
        ],
    );
    window.receive();
    assert!(window.failed().is_none(), "{:?}", window.failed());
    assert_eq!(front(&mut window), Some(NavPane::Overview));
}

/// The list of the view's overview.
fn overview_list(window: &mut AppWindow) -> &mut ui::widget::DescribedList {
    window
        .shell_mut()
        .and_then(|shell| shell.overview_list_mut())
        .expect("the overview's list")
}

#[test]
fn the_row_of_the_view_beside_the_list_is_selected_once_the_rows_arrive() {
    let (mut app, mut window) = window();
    send(&mut app, &list_page());
    // The view beside the list opens before the app has rendered any of the list's rows: the
    // list cannot name the row of the view it shows until they come.
    send(&mut app, &opened_note("a", "Alpha"));
    window.receive();
    assert!(
        window.failed().is_none(),
        "the views were taken: {:?}",
        window.failed()
    );
    send(
        &mut app,
        &[ToUi::Rows {
            lane: Lane::Root,
            list: "list".to_owned(),
            len: 2,
            start: 0,
            keys: vec!["notes/a".to_owned(), "notes/b".to_owned()],
            rows: vec![
                UiNode::empty("ui.ListItem", "notes/a"),
                UiNode::empty("ui.ListItem", "notes/b"),
            ],
        }],
    );
    window.receive();
    assert!(
        window.failed().is_none(),
        "the rows were taken: {:?}",
        window.failed()
    );
    let list = overview_list(&mut window)
        .list()
        .expect("the overview's list");
    let selection = list.selection();
    assert!(
        selection.is_selected(&"notes/a".to_owned()),
        "the row of the view shown is the selected one"
    );
    assert!(!selection.is_selected(&"notes/b".to_owned()), "only it is");
}

/// A shell node with `slots`, each `(name, children)`, as a view's tree is.
fn shell_node(key: &str, props: &[(&str, Value)], slots: Vec<(&str, Vec<UiNode>)>) -> UiNode {
    let mut shell = UiNode::empty(&kind("NavShell"), key);
    for (name, value) in props {
        shell.props.insert((*name).to_owned(), value.clone());
    }
    for (name, children) in slots {
        let mut slot = UiNode::empty(SLOT, &format!("{key}#{name}"));
        slot.props.insert("name".to_owned(), Value::text(name));
        slot.children = children;
        shell.children.push(slot);
    }
    shell
}

/// A button node of a header.
fn header_button(key: &str, label: &str, role: &str) -> UiNode {
    let mut button = UiNode::empty(&kind("Button"), key);
    button.props.insert("label".to_owned(), Value::text(label));
    if !role.is_empty() {
        button
            .props
            .insert("role".to_owned(), Value::some(Value::text(role)));
    }
    button
        .events
        .insert("press".to_owned(), wire::Event::default());
    button
}

/// A list node, as a view draws the column of what it shows.
fn list_node(key: &str) -> UiNode {
    UiNode::empty(&kind("VirtualList"), key)
}

/// Where the thing a person sees as `name` is drawn, in window coordinates.
fn bounds_named(harness: &ui::headless::FrameHarness, name: &str) -> ui::kurbo::Rect {
    harness
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.label() == Some(name))
                .then(|| {
                    node.bounds()
                        .map(|b| ui::kurbo::Rect::new(b.x0, b.y0, b.x1, b.y1))
                })
                .flatten()
        })
        .unwrap_or_else(|| {
            let drawn: Vec<String> = harness
                .a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| node.label().map(str::to_owned))
                .collect();
            panic!("{name:?} is drawn: {drawn:?}")
        })
}

/// The window after the root's that draws a list of its own moves the window there: its list's
/// actions stand in the window's one bar from the frame that shows them, placed where a person
/// presses them, and the list's title is the window's.
#[test]
fn the_list_a_window_moves_to_brings_its_actions_placed_into_the_one_bar() {
    let root = shell_node(
        "shell",
        &[("overview_title", Value::text("Inbox"))],
        vec![
            ("overview", vec![list_node("inbox")]),
            (
                "overview_actions",
                vec![
                    header_button("new-task", "New task", "primary"),
                    header_button("new-project", "New project", ""),
                ],
            ),
            ("content", vec![UiNode::empty("stack", "none")]),
        ],
    );
    let (mut app, mut harness) = drawn(&[ToUi::Open {
        lane: Lane::Root,
        target: doc(),
        tree: root,
        front: None,
    }]);
    assert!(
        drawn_window(&mut harness).failed().is_none(),
        "the window opened: {:?}",
        drawn_window(&mut harness).failed()
    );
    assert!(
        bounds_named(&harness, "New task").width() > 0.0,
        "the window's own primary stands in its bar"
    );
    // The app opens a project beside the list: its view draws a list of its own.
    let project = shell_node(
        "project",
        &[("overview_title", Value::text("Move flat"))],
        vec![
            ("overview", vec![list_node("tasks")]),
            (
                "overview_actions",
                vec![
                    header_button("add", "Add task", "primary"),
                    header_button("section", "New section", ""),
                ],
            ),
            ("content", vec![UiNode::empty("stack", "none")]),
        ],
    );
    send(
        &mut app,
        &[ToUi::Open {
            lane: Lane::Detail,
            target: note("p"),
            tree: project,
            front: None,
        }],
    );
    drawn_window(&mut harness).receive();
    harness.relayout();
    let added = bounds_named(&harness, "Add task");
    assert!(
        added.width() > 0.0 && added.y0 < 56.0,
        "the project's primary stands in the window's bar, placed: {added:?}"
    );
    let section = bounds_named(&harness, "New section");
    assert!(
        section.width() > 0.0 && section.y0 < 56.0,
        "the project's other action stands in the bar, placed: {section:?}"
    );
}
/// The node of `kind` in `tree`, to change.
fn node_of<'a>(tree: &'a mut UiNode, kind: &str) -> Option<&'a mut UiNode> {
    if tree.kind == kind {
        return Some(tree);
    }
    tree.children
        .iter_mut()
        .find_map(|child| node_of(child, kind))
}

/// The rows of a list the app renders, each showing the item `id` under `title`, as a loop
/// over the items of a resource sends them.
fn listed_rows(items: &[(&str, &str)]) -> Vec<ToUi> {
    let mut keys = Vec::new();
    let mut rows = Vec::new();
    for (id, title) in items {
        let mut row = UiNode::empty(&kind("ListRow"), &format!("rows/{id}#row"));
        row.props.insert("title".to_owned(), Value::text(*title));
        let mut item = UiNode::empty(&kind("ListItem"), &format!("rows/{id}"));
        item.props.insert(
            "item".to_owned(),
            Value::Ref(ResourceRef {
                id: (*id).to_owned(),
                ty: "tasks.Task".to_owned(),
            }),
        );
        item.children.push(row);
        keys.push(format!("rows/{id}"));
        rows.push(item);
    }
    vec![ToUi::Rows {
        lane: Lane::Root,
        list: "list".to_owned(),
        len: u32::try_from(keys.len()).expect("a few rows"),
        start: 0,
        keys,
        rows,
    }]
}

/// A window whose own list shows an item as its page is named by the item, as its row names it
/// — a task opened from search reads "Pay rent" in the desktop's tab — and by its page again once
/// the list shows none.
#[test]
fn a_window_showing_an_item_as_its_page_is_named_by_the_item() {
    let (mut app, mut window) = window();
    let page = list_page();
    let Some(ToUi::Open { tree, .. }) = page.first() else {
        panic!("the page opens the window");
    };
    let mut opened = tree.clone();
    send(&mut app, &page);
    window.receive();
    send(
        &mut app,
        &listed_rows(&[
            ("root/tasks:t1", "Call the landlord"),
            ("root/tasks:t2", "Pay rent"),
        ]),
    );
    window.receive();
    let unopened = window.title().map(str::to_owned);
    assert_ne!(unopened.as_deref(), Some("Pay rent"));
    node_of(&mut opened, &kind("VirtualList"))
        .expect("the page's list")
        .props
        .insert(
            "open".to_owned(),
            Value::Ref(ResourceRef {
                id: "root/tasks:t2".to_owned(),
                ty: "tasks.Task".to_owned(),
            }),
        );
    send(
        &mut app,
        &[ToUi::Open {
            lane: Lane::Root,
            target: doc(),
            tree: opened,
            front: None,
        }],
    );
    window.receive();
    send(
        &mut app,
        &listed_rows(&[
            ("root/tasks:t1", "Call the landlord"),
            ("root/tasks:t2", "Pay rent"),
        ]),
    );
    window.receive();
    assert!(window.failed().is_none(), "{:?}", window.failed());
    assert_eq!(
        window.title(),
        Some("Pay rent"),
        "named by the item it shows"
    );
    send(&mut app, &page);
    window.receive();
    assert_eq!(
        window.title().map(str::to_owned),
        unopened,
        "named by its page again"
    );
}

/// A choice of several rows is the person's own while it holds: the rows the app sends again
/// after a press leave it as it is, and only the host's own putting away of the choice ends it.
#[test]
fn a_choice_of_several_rows_outlasts_the_rows_the_app_sends_again() {
    use ui::widget::ListModel;
    let (mut app, mut window) = window();
    let mut page = list_page();
    let Some(ToUi::Open { tree, .. }) = page.first_mut() else {
        panic!("the page opens the window");
    };
    node_of(tree, &kind("VirtualList"))
        .expect("the page's list")
        .props
        .insert("multi_select".to_owned(), Value::Bool(true));
    send(&mut app, &page);
    window.receive();
    let items = [
        ("root/tasks:t1", "Call the landlord"),
        ("root/tasks:t2", "Pay rent"),
    ];
    send(&mut app, &listed_rows(&items));
    window.receive();
    assert!(
        window.failed().is_none(),
        "the rows were taken: {:?}",
        window.failed()
    );
    let list = overview_list(&mut window)
        .list_mut()
        .expect("the overview's list");
    let (first, second) = (
        "rows/root/tasks:t1".to_owned(),
        "rows/root/tasks:t2".to_owned(),
    );
    assert!(
        list.model().index_of(&first).is_some() && list.model().index_of(&second).is_some(),
        "the list holds the rows the app sent"
    );
    list.selection_mut().select_only(first.clone());
    list.selection_mut().toggle(second.clone());
    assert_eq!(
        list.selection().len(list.model().len()),
        2,
        "both rows are chosen"
    );

    // The app sends its rows again, as it does after every press: the choice stays.
    send(&mut app, &listed_rows(&items));
    window.receive();
    let list = overview_list(&mut window)
        .list()
        .expect("the overview's list");
    let mut kept: Vec<String> = list.selection().stored_keys().cloned().collect();
    kept.sort();
    let mut want = vec![first, second];
    want.sort();
    assert_eq!(
        kept, want,
        "the choice of several rows outlasts the rows the app sends again"
    );

    // What the host kept of the choice gone — an action over the rows ran — the list puts its
    // own away with it.
    send(
        &mut app,
        &[ToUi::Patch {
            lane: Lane::Root,
            patches: vec![wire::Patch::SetProp {
                path: vec![
                    "overview".to_owned(),
                    "docked".to_owned(),
                    "list".to_owned(),
                ],
                name: "chosen".to_owned(),
                value: Some(Value::List(Vec::new())),
            }],
        }],
    );
    window.receive();
    assert!(
        window.failed().is_none(),
        "the choice was put away: {:?}",
        window.failed()
    );
    assert_eq!(
        overview_list(&mut window)
            .list()
            .expect("the overview's list")
            .selection()
            .len(2),
        0,
        "the choice the host put away is put away in the list"
    );
}

/// A page whose overview list lets a person choose several rows, as `std.list` declares
/// it, reporting every change of the choice to the app.
fn choose_several_page() -> Vec<ToUi> {
    let mut page = list_page();
    if let Some(ToUi::Open { tree, .. }) = page.first_mut() {
        let list = node_of(tree, &kind("VirtualList")).expect("the page's list");
        list.props
            .insert("multi_select".to_owned(), Value::Bool(true));
        list.events
            .insert("selected".to_owned(), wire::Event::default());
    }
    page
}

/// The choices the app was told of, in order: what each report of the list's selection held.
fn choices(app: &mut local::End) -> Vec<Option<Value>> {
    sent(app)
        .into_iter()
        .filter_map(|message| match message {
            ToApp::Fire { event, value, .. } if event == "selected" => Some(value),
            _ => None,
        })
        .collect()
}

/// The rows a report of the list's choice holds, as the keys of the rows it names.
fn chosen_rows(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::List(items)) => items
            .iter()
            .map(|item| match item {
                Value::Ref(reference) => reference.id.clone(),
                other => panic!("a chosen row is a reference, not {other:?}"),
            })
            .collect(),
        other => panic!("a choice is a list of rows, not {other:?}"),
    }
}

/// Where the row `key` stands once the frames so far have laid it out.
fn centre_of(harness: &mut ui::headless::FrameHarness, key: &str) -> Option<(f64, f64)> {
    let centre = drawn_window(harness)
        .shell_mut()
        .and_then(|shell| shell.overview_list_mut())
        .and_then(|list| list.list())
        .and_then(|list| list.row_rect(&key.to_owned()))?
        .center();
    Some((centre.x, centre.y))
}

/// A row clicked plainly, another added with Control held, then Shift and the arrow: the walk
/// extends the choice the person gathered and the window reports it, so the bar standing for
/// the choice grows with it.
#[test]
fn a_ctrl_click_then_shift_down_extends_the_choice() {
    let (mut app, mut window) = window();
    send(&mut app, &choose_several_page());
    window.receive();
    let items = [
        ("root/tasks:t1", "Call the landlord"),
        ("root/tasks:t2", "Pay rent"),
        ("root/tasks:t3", "Plan the trip"),
    ];
    let mut harness =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (1280, 800));
    // The window asks its app for the rows it shows, as it does over a socket: the app reads
    // the list's children anew (`RowsChanged` with a `Reset`), then renders the rows.
    for _ in 0..8 {
        harness.recompose().expect("a frame");
        drawn_window(&mut harness).flush();
        if !answer_rows(&mut app, &items) {
            continue;
        }
        drawn_window(&mut harness).receive();
        harness.recompose().expect("a frame");
        if centre_of(&mut harness, "rows/root/tasks:t1").is_some() {
            break;
        }
    }

    let (first, second, third) = (
        "rows/root/tasks:t1",
        "rows/root/tasks:t2",
        "rows/root/tasks:t3",
    );
    // A plain click puts the keyboard in the window and chooses the row it lands on.
    let (x, y) = centre_of(&mut harness, first).expect("the first row is laid out");
    harness.click_at(x, y);
    drawn_window(&mut harness).flush();
    let choice = choices(&mut app).pop().expect("the click reports");
    assert_eq!(
        chosen_rows(choice.as_ref()),
        ["root/tasks:t1"],
        "the click begins the choice at its row"
    );
    // Control held for the next press, as the person holds it, adds the row it lands on.
    let (x, y) = centre_of(&mut harness, second).expect("the second row is laid out");
    harness.set_modifiers(&["ctrl".to_owned()]);
    harness.click_at(x, y);
    harness.set_modifiers(&[]);
    drawn_window(&mut harness).flush();
    let choice = choices(&mut app).pop().expect("the modified click reports");
    assert_eq!(
        chosen_rows(choice.as_ref()),
        ["root/tasks:t1", "root/tasks:t2"],
        "the modified click adds to the choice"
    );
    // Shift and the arrow below: the walk extends the choice the person gathered.
    press(&mut harness, &["shift"], "ArrowDown");
    let named = |key: &str| key.trim_start_matches("rows/").to_owned();
    let choice = choices(&mut app).pop().expect("the walk reports");
    assert_eq!(
        chosen_rows(choice.as_ref()),
        [named(first), named(second), named(third)],
        "the walk extends the choice instead of replacing it"
    );
}

/// Answer what the window asked its app for with `items`, as an app renders them: whether it
/// asked for rows at all.
fn answer_rows(app: &mut local::End, items: &[(&str, &str)]) -> bool {
    let mut asked = false;
    for message in sent(app) {
        if matches!(message, ToApp::Rows { .. }) {
            asked = true;
        }
    }
    if asked {
        send(
            app,
            &[ToUi::RowsChanged {
                lane: Lane::Root,
                list: "list".to_owned(),
                len: u32::try_from(items.len()).expect("a few rows"),
                changes: vec![wire::RowChange::Reset],
            }],
        );
        send(app, &listed_rows(items));
    }
    asked
}

/// A list window: a shell whose overview lists rows, declaring where the window's search goes.
fn searching_list_page(rows: i64) -> Vec<ToUi> {
    let mut list = UiNode::empty("ui.VirtualList", "list");
    list.props.insert("in_column".to_owned(), Value::Bool(true));
    let mut overview = UiNode::empty(SLOT, "shell#overview");
    overview
        .props
        .insert("name".to_owned(), Value::text("overview"));
    overview.children.push(list);
    let mut shell = shell_node(
        "shell",
        &[
            ("content_kind", Value::text("document")),
            ("overview_title", Value::text("All notes")),
            ("overview_count", Value::Int(rows)),
        ],
        vec![("overview", vec![overview])],
    );
    shell
        .events
        .insert("search".to_owned(), crate::Event::default());
    vec![
        ToUi::Open {
            lane: Lane::Root,
            target: doc(),
            tree: shell,
            front: None,
        },
        ToUi::Rows {
            lane: Lane::Root,
            list: "list".to_owned(),
            len: u32::try_from(rows).expect("a list's length"),
            start: 0,
            keys: vec!["notes/a".to_owned()],
            rows: vec![UiNode::empty("ui.ListItem", "notes/a")],
        },
    ]
}

#[test]
fn the_window_s_search_narrows_the_list_its_frame_declares() {
    use ui::widget::{InPlace, Queryable, SearchDriver};
    let (mut app, mut window) = window();
    send(&mut app, &searching_list_page(40));
    window.receive();
    assert_eq!(
        InPlace::of(&mut window),
        InPlace::Query,
        "the frame says where the search goes, so the bar narrows the list"
    );
    let mut driver = SearchDriver::default();
    driver.open(InPlace::Query);
    let touched = driver.drive(
        &mut window,
        "pla",
        ui::widget::FindOptions::default(),
        None,
        false,
    );
    assert!(touched, "the search reached the app");
    window.flush();
    let Some(ToApp::Fire {
        event,
        key,
        value: Some(value),
        ..
    }) = told(&mut app).pop()
    else {
        panic!("the window told the app what the search holds");
    };
    assert_eq!((event.as_str(), key.as_str()), ("search", "shell"));
    assert_eq!(value, Value::text("pla"));
    // The app answers with the rows the search left, and the window counts them of the rows
    // the list had before anything was typed.
    send(
        &mut app,
        &[ToUi::RowsChanged {
            lane: Lane::Root,
            list: "list".to_owned(),
            len: 3,
            changes: Vec::new(),
        }],
    );
    window.receive();
    assert_eq!(
        Queryable::query_count(&window),
        Some(ui::widget::FilterCount::new(3, 40)),
        "3 of 40"
    );
    // Closing the search widens the list again.
    driver.drive(
        &mut window,
        "",
        ui::widget::FindOptions::default(),
        None,
        true,
    );
    window.flush();
    assert!(
        told(&mut app).iter().any(|message| matches!(
            message,
            ToApp::Fire { event, value: Some(value), .. }
                if event == "search" && *value == Value::text("")
        )),
        "closing the search widens the list again"
    );
    assert_eq!(
        InPlace::of(&mut window),
        InPlace::Query,
        "the search closes, the frame stays"
    );
}

/// A window over one record, with the record's `new` action in its bar: nothing listed, or a
/// list of one thing — the composition that moves the action out of the bar into the columns'
/// header row.
fn action_shell(listing: bool) -> Vec<ToUi> {
    use wire::Event;
    let slot = |name: &str| {
        let mut node = UiNode::empty(SLOT, &format!("shell#{name}"));
        node.props.insert("name".to_owned(), Value::text(name));
        node
    };
    let mut button = UiNode::empty(&kind("Button"), "shell#new");
    button
        .props
        .insert("label".to_owned(), Value::text("New note"));
    button.events.insert(
        "press".to_owned(),
        Event {
            invokes: Some(("1".to_owned(), "new".to_owned())),
            ..Event::default()
        },
    );
    let mut actions = slot("actions");
    actions.children.push(button);
    let mut overview = slot("overview");
    if listing {
        let mut row = UiNode::empty(&kind("ListRow"), "shell#row");
        row.props.insert("title".to_owned(), Value::text("First"));
        overview.children.push(row);
    } else {
        let mut state = UiNode::empty(&kind("EmptyState"), "shell#none");
        state
            .props
            .insert("title".to_owned(), Value::text("No notes yet"));
        overview.children.push(state);
    }
    let mut content = slot("content");
    let mut unopened = UiNode::empty(&kind("EmptyState"), "shell#unopened");
    unopened
        .props
        .insert("title".to_owned(), Value::text("No note open"));
    content.children.push(unopened);
    let mut shell = UiNode::empty(&kind("NavShell"), "shell");
    shell.children.extend([overview, content, actions]);
    let mut target = doc();
    target.id = "1".to_owned();
    target.ty = "test.tasks".to_owned();
    vec![
        ToUi::Open {
            lane: Lane::Root,
            target,
            tree: shell,
            front: None,
        },
        ToUi::Actions(wire::ActionCatalogue::All(catalogue_of("New note", None))),
    ]
}

/// What the window's tree says the control called `name` does.
fn identity_named(harness: &ui::headless::FrameHarness, name: &str) -> Option<String> {
    harness
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .and_then(|(_, node)| node.author_id().map(str::to_owned))
}

/// The accessibility node the window describes the control `name` in.
fn named_node(harness: &ui::headless::FrameHarness, name: &str) -> ui::accesskit::NodeId {
    harness
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("the window draws {name:?}"))
}

#[test]
fn the_first_item_moves_the_windows_action_and_not_what_it_does() {
    let (mut app, mut window) = window();
    send(&mut app, &action_shell(false));
    window.receive();
    let mut harness =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (1280, 800));
    harness.relayout();
    let identity = "test.tasks\u{1f}new";
    assert_eq!(
        identity_named(&harness, "New note").as_deref(),
        Some(identity),
        "the bar's button says what it does: {:?}",
        harness
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| Some((node.label()?.to_owned(), node.author_id()?.to_owned())))
            .collect::<Vec<_>>()
    );
    let in_the_bar = named_node(&harness, "New note");

    // The first note: the window's columns come back, and the action stands in their header row.
    send(&mut app, &action_shell(true));
    drawn_window(&mut harness).receive();
    harness.relayout();
    let in_the_columns = named_node(&harness, "New note");
    assert_ne!(
        in_the_bar, in_the_columns,
        "the window drew the control again, so the node id found before it is gone"
    );
    assert_eq!(
        identity_named(&harness, "New note").as_deref(),
        Some(identity),
        "and what it does is where it was, whatever the window composed around it"
    );
    // One control of one action drawn in two places keeps a node each, so no two nodes of the
    // tree share an id.
    let ids: Vec<u64> = harness
        .a11y_nodes()
        .into_iter()
        .map(|(id, _)| id.0)
        .collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "every node keeps an id of its own");
}
