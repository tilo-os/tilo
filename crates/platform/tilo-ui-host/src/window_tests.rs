//! The window's tests: what a person and an app see of one window.

use super::*;

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

fn send(app: &mut tilo_render_protocol::patch::local::End, batch: &[ToUi]) {
    app.send(wire::encode_batch(batch).expect("encodes"))
        .expect("sends");
}

fn sent(app: &mut tilo_render_protocol::patch::local::End) -> Vec<ToApp> {
    let mut out = Vec::new();
    while let Some(batch) = app.recv().expect("receives") {
        out.extend(wire::decode_batch::<ToApp>(&batch).expect("a batch"));
    }
    out
}

/// The actions a press asked the page to run.
fn fired(app: &mut tilo_render_protocol::patch::local::End) -> Vec<ToApp> {
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
fn spaces(app: &mut tilo_render_protocol::patch::local::End) -> Vec<(Lane, f32, f32)> {
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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

    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
    use wire::Event;
    let mut button = UiNode::empty("ui.Button", "archive");
    button
        .props
        .insert("label".to_owned(), Value::text("Archive"));
    button.props.insert("key".to_owned(), Value::text(key));
    button.events.insert("press".to_owned(), Event::default());
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
    send(&mut app, &keyed_menu("Ctrl+Enter"));
    window.receive();
    assert!(
        window.failed().is_none(),
        "the view was taken: {:?}",
        window.failed()
    );
    assert!(
        window
            .composed()
            .is_some_and(|tree| shortcut_action_declared(
                tree,
                &ui::platform::Shortcut::from("Ctrl+Enter")
            )
            .is_some()),
        "the composed shell carries the declared shortcut: {:#?}",
        window.composed()
    );
    let mut harness =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (900, 700));
    let press = |harness: &mut ui::headless::FrameHarness, modifiers: &[&str], key: &str| {
        let modifiers: Vec<String> = modifiers.iter().map(|name| (*name).to_owned()).collect();
        harness.set_modifiers(&modifiers);
        harness.key(key);
        harness.set_modifiers(&[]);
        let window = harness
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AppWindow>())
            .expect("the window");
        window.flush();
    };
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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

/// A page whose one described control `kind` is bound to a note's field and shows `value`,
/// with `props` as the view set them.
fn bound_page(kind: &str, props: &[(&str, Value)], value: Value) -> Vec<ToUi> {
    let mut control = UiNode::empty(kind, "field");
    control.binding = Some(crate::Binding {
        field: Some(("doc/notes:a".to_owned(), "field".to_owned())),
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
fn drawn(
    page: &[ToUi],
) -> (
    tilo_render_protocol::patch::local::End,
    ui::headless::FrameHarness,
) {
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
fn told(app: &mut tilo_render_protocol::patch::local::End) -> Vec<ToApp> {
    sent(app)
        .into_iter()
        .filter(|message| !matches!(message, ToApp::Space { .. }))
        .collect()
}

/// The accessibility node of the control named `name`.
fn node_named(harness: &ui::headless::FrameHarness, name: &str) -> ui::accesskit::NodeId {
    harness
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("{name:?} is drawn"))
}

/// A value set elsewhere reaches the control showing it, whatever the control is.
#[test]
fn a_bound_control_shows_the_value_the_app_bound() {
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
    let day = shown(
        "ui.DateField",
        &labelled,
        Value::some(Value::text("2026-06-30")),
        &ui::widget::DateField::label(chrono::NaiveDate::from_ymd_opt(2026, 6, 30)),
    );
    assert_eq!(
        day.label(),
        Some("Jun 30, 2026"),
        "the day the app bound is the day the field shows"
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
}

/// A day picked in the calendar reaches the app as the value the field now holds, not as a
/// splice of the text it reads as.
#[test]
fn a_picked_day_is_sent_as_the_day_the_field_now_holds() {
    let (mut app, mut harness) = drawn(&bound_page(
        "ui.DateField",
        &[
            ("label", Value::text("Due")),
            ("optional", Value::Bool(true)),
        ],
        Value::some(Value::text("2026-06-30")),
    ));
    let field = node_named(&harness, "Jun 30, 2026");
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
        .filter(|label| label == "Jul 1, 2026");
    assert_eq!(
        shown,
        Some("Jul 1, 2026".to_owned()),
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
        .filter(|label| label == "Jul 5, 2026");
    assert_eq!(
        shown,
        Some("Jul 5, 2026".to_owned()),
        "a day the app set later shows in place"
    );
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

/// A page whose overview is one list over the rows the app renders, as `std.rows` builds it:
/// the list inside the page an app docks its composer or bar under.
fn list_page() -> Vec<ToUi> {
    let mut list = UiNode::empty("ui.VirtualList", "list");
    list.props.insert("in_column".to_owned(), Value::Bool(true));
    let mut docked = UiNode::empty("ui.Docked", "docked");
    docked.children.push(list);
    let mut overview = UiNode::empty("slot", "overview");
    overview
        .props
        .insert("name".to_owned(), Value::text("overview"));
    overview.children.push(docked);
    let mut shell = UiNode::empty("ui.NavShell", "shell");
    shell.children.push(overview);
    vec![ToUi::Open {
        lane: Lane::Root,
        target: doc(),
        tree: shell,
        front: None,
    }]
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
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
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
