//! The window's tests: what a person and an app see of one window.

use super::*;
use crate::adapter::{PART, kind};
use tilo_render_protocol::patch::local;

/// `base` edited to `ours` and, meanwhile, to `theirs`: what the text holds once ours is rebased
/// over theirs.
fn merged(base: &str, ours: &str, theirs: &str) -> String {
    let (at, delete, insert) =
        rebase_splice(splice_between(base, ours), splice_between(base, theirs));
    let mut text = theirs.to_owned();
    text.replace_range(at..at.saturating_add(delete), &insert);
    text
}

#[test]
fn held_typing_is_rebased_over_what_the_app_changed_since_it_was_typed() {
    // Typed at the end while the app changed nothing: as typed.
    assert_eq!(
        merged("Q3 summari", "Q3 summarise", "Q3 summari"),
        "Q3 summarise"
    );
    // The app inserted before the typing: the typing keeps its place in the text.
    assert_eq!(merged("plan", "plans", "Q3 plan"), "Q3 plans");
    // The app inserted after it: untouched.
    assert_eq!(merged("plan", "Xplan", "plan v2"), "Xplan v2");
    // Both inserted at one place: theirs first, ours after.
    assert_eq!(merged("", "abc", "note"), "noteabc");
    // The entry emptied itself after a submit the app took and emptied too: nothing left to do.
    assert_eq!(merged("New task", "", ""), "");
    // The person deleted a word and its space, the app replaced the word: the app's word stands,
    // the space the person deleted goes.
    assert_eq!(
        merged("alpha beta gamma", "alpha gamma", "alpha B gamma"),
        "alpha Bgamma"
    );
    // The person deleted across what the app inserted into: the rest of their delete goes.
    assert_eq!(merged("abcdef", "af", "abXcdef"), "aXf");
    // Multi-byte text keeps whole characters.
    assert_eq!(merged("日本", "日本語", "新日本"), "新日本語");
}

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
        text.replace_range(at..at.saturating_add(delete), &insert);
        assert_eq!(text, after);
    }
}

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

fn fired(app: &mut local::End) -> Vec<ToApp> {
    sent(app)
        .into_iter()
        .filter(|message| matches!(message, ToApp::Fire { .. }))
        .collect()
}

fn doc() -> ResourceRef {
    ResourceRef {
        id: "doc".to_owned(),
        ty: "notes.Note".to_owned(),
    }
}

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
            level: Level::Root,
            target: doc(),
            tree: page,
            front: None,

            capture_protected: false,
        },
        ToUi::Bound {
            level: Level::Root,
            key: "title".to_owned(),
            value: Value::text("Q3"),
            acked: 0,
        },
    ]
}

fn spaces(app: &mut local::End) -> Vec<(Level, String, f32, f32)> {
    sent(app)
        .into_iter()
        .filter_map(|message| match message {
            ToApp::Space {
                level,
                key,
                width,
                height,
                ..
            } => Some((level, key, width, height)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_view_is_told_the_space_layout_gave_it_when_it_changes_by_more_than_a_step() {
    let (mut app, mut window) = window();
    send(&mut app, &titled_page());
    window.receive();
    let mut harness = ui::headless::Driver::new(ui::State::new(), Box::new(window), (390, 844));
    let flush = |harness: &mut ui::headless::Driver| {
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
    let [(Level::Root, _, width, height)] = told[..] else {
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
        matches!(told[..], [(Level::Root, _, width, _)] if (f64::from(width) * rem - 1280.0).abs() < 1.0),
        "{told:?}"
    );
}

#[test]
fn typing_is_sent_as_splices_and_what_the_app_sent_before_it_saw_them_is_not_shown() {
    let (mut app, mut window) = window();
    send(&mut app, &titled_page());
    window.receive();
    assert_eq!(window.bound_value("title"), Some(Value::text("Q3")));
    // The person types: the field shows it at once (it is the toolkit's), and the app hears of it
    // as a splice of what the field showed.
    window.act(UiEvent::Edit("title".to_owned(), "Q3 plan".to_owned()));
    window.flush();
    assert_eq!(
        sent(&mut app),
        [ToApp::Splice {
            level: Level::Root,
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
            level: Level::Root,
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
            level: Level::Root,
            key: "title".to_owned(),
            value: Value::text("Q3 plans"),
            acked: 2,
        }],
    );
    window.receive();
    assert_eq!(window.bound_value("title"), Some(Value::text("Q3 plans")));
}

fn note(id: &str) -> ResourceRef {
    ResourceRef {
        id: id.to_owned(),
        ty: "notes.Note".to_owned(),
    }
}

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
            level: Level::Detail,
            target: note(id),
            tree: page,
            front: Some(wire::Front::Input),

            capture_protected: false,
        },
        ToUi::Bound {
            level: Level::Detail,
            key: "title".to_owned(),
            value: Value::text(title),
            acked: 0,
        },
    ]
}

#[test]
fn a_batch_that_breaks_the_protocol_shows_the_error_state_and_nothing_else() {
    let (mut app, mut window) = window();
    let unknown = vec![ToUi::Open {
        level: Level::Root,
        target: ResourceRef {
            id: "doc".to_owned(),
            ty: "app.Root".to_owned(),
        },
        tree: UiNode::empty("ui.NoSuchComponent", "root"),
        front: None,
        capture_protected: false,
    }];
    app.send(wire::encode_batch(&unknown).expect("encodes"))
        .expect("sends");
    assert!(window.receive());
    assert!(window.failed().is_some());
    assert!(window.composed().is_none(), "nothing of it was built");
    app.send(b"scribbles".to_vec()).expect("sends");
    assert!(window.receive(), "a scribble is refused, not a panic");
}

fn catalogue_of(label: &str, shortcut: Option<&str>) -> Vec<u8> {
    catalogue_on("tasks:1", label, shortcut)
}

/// The published catalogue of `resource`'s one action, as the window receives it.
fn catalogue_on(resource: &str, label: &str, shortcut: Option<&str>) -> Vec<u8> {
    crate::catalogue::whole(&actions_on(resource, label, shortcut))
}

/// The one action `label` with `shortcut` of the resource `resource`, as the app surveys it.
fn actions_on(
    resource: &str,
    label: &str,
    shortcut: Option<&str>,
) -> Vec<tilo_actions::ActionDescriptor> {
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
    let resource = ResourceSpec::new(resource, "test.tasks", "Tasks").with_action(spec);
    tilo_actions::api::survey(&[resource])
}

/// The window offers the actions its app publishes, as each change of its surface leaves
/// them: a change replaces the actions of the resources it names, with the key the app gives
/// them now, adds those of a resource that joined after the rest, and drops those of one the
/// app no longer offers.
#[test]
fn a_published_catalogue_is_what_the_windows_palette_offers() {
    let (mut app, mut window) = window();
    let offered = |window: &AppWindow| {
        window
            .actions()
            .iter()
            .map(|action| {
                (
                    action.address.resource.clone(),
                    action.action_label.clone(),
                    action.state.shortcut.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let one = |resource: &str, label: &str, key: &str| {
        (resource.to_owned(), label.to_owned(), Some(key.to_owned()))
    };
    // The app's surface, as the broker and the window hold it, changed one step at a time.
    let mut held: Option<(
        Vec<tilo_actions::ActionDescriptor>,
        ui::collaboration::SurfacePublication,
    )> = None;
    let mut publish = |app: &mut local::End, offers: Vec<tilo_actions::ActionDescriptor>| {
        let (delta, state) = ui::collaboration::SurfacePublication::next(
            held.as_ref()
                .map(|(descriptors, state)| (state, descriptors.as_slice())),
            &offers,
        );
        held = Some((offers, state));
        send(
            app,
            &[ToUi::Actions(serde_json::to_vec(&delta).expect("encodes"))],
        );
    };
    publish(&mut app, actions_on("tasks:1", "New task", Some("Ctrl+n")));
    window.receive();
    assert_eq!(offered(&window), [one("tasks:1", "New task", "Ctrl+n")]);
    publish(
        &mut app,
        actions_on("tasks:1", "Create task", Some("Ctrl+enter")),
    );
    window.receive();
    assert_eq!(
        offered(&window),
        [one("tasks:1", "Create task", "Ctrl+enter")],
        "one action, relabeled in place"
    );
    publish(
        &mut app,
        [
            actions_on("tasks:2", "Add", Some("Ctrl+a")),
            actions_on("tasks:1", "Create task", Some("Ctrl+enter")),
        ]
        .concat(),
    );
    window.receive();
    assert_eq!(
        offered(&window),
        [
            one("tasks:1", "Create task", "Ctrl+enter"),
            one("tasks:2", "Add", "Ctrl+a"),
        ],
        "a resource that joined is offered after those offered before it"
    );
    publish(&mut app, actions_on("tasks:1", "New task", Some("Ctrl+n")));
    window.receive();
    assert_eq!(
        offered(&window),
        [one("tasks:1", "New task", "Ctrl+n")],
        "a resource the app no longer offers takes its actions with it"
    );
}

/// The window declares each published action as the app declared it — what it takes, and whether it
/// can run now — because that is what its palette lists by.
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
    let published = crate::catalogue::whole(&tilo_actions::api::survey(&[resource]));

    let (mut app, mut window) = window();
    send(&mut app, &[ToUi::Actions(published)]);
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
    let mut actions = UiNode::empty("part", "actions");
    actions
        .props
        .insert("name".to_owned(), Value::text("actions"));
    actions.children.push(button);
    shell.children.push(actions);
    vec![ToUi::Open {
        level: Level::Root,
        target: doc(),
        tree: shell,
        front: None,
        capture_protected: false,
    }]
}

/// A key that presses a control says where in the window that control stands: a question the
/// call it makes raises stands there, as one a click raises stands where the click was. A key the
/// window passes to the control the keyboard is in says where that control stands.
#[test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "a control's place in a window is a few thousand pixels at most"
)]
fn a_key_that_presses_a_control_says_where_the_control_stands() {
    const DECLARED: u32 = 41;
    const FOCUSED: u32 = 42;
    let point = |rect: ui::kurbo::Rect| {
        let centre = rect.center();
        tilo_actions::remote::asked::PressPoint {
            x: centre.x.round() as i32,
            y: centre.y.round() as i32,
        }
    };
    let mut archive = UiNode::empty("ui.Button", "archive");
    archive
        .props
        .insert("label".to_owned(), Value::text("Archive"));
    archive
        .props
        .insert("key".to_owned(), Value::text("Ctrl+Enter"));
    archive
        .events
        .insert("press".to_owned(), wire::Event::default());
    let mut page = UiNode::empty("stack", "page");
    page.children.push(archive);
    let (mut app, mut harness) = drawn(&[ToUi::Open {
        level: Level::Root,
        target: doc(),
        tree: page,
        front: None,
        capture_protected: false,
    }]);
    let button = rect_of(drawn_window(&mut harness).body.as_ref(), "archive")
        .expect("the button is laid out");
    ui::press::note(DECLARED);
    press(&mut harness, &["command"], "Enter");
    assert_eq!(fired(&mut app).len(), 1, "the key pressed the button");
    assert_eq!(
        tilo_actions::remote::press_points::of(DECLARED),
        Some(point(button)),
        "the key's press stands at the button it pressed"
    );

    let (_app, mut harness) = drawn(&bound_page("ui.TextInput", &[], Value::text("hello")));
    harness.click_named("field").expect("the field is drawn");
    harness.relayout();
    let field =
        rect_of(drawn_window(&mut harness).body.as_ref(), "field").expect("the field is laid out");
    ui::press::note(FOCUSED);
    harness.key("Enter");
    assert_eq!(
        tilo_actions::remote::press_points::of(FOCUSED),
        Some(point(field)),
        "a key no control declares stands at the control the keyboard is in"
    );
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
    let mut harness = ui::headless::Driver::new(ui::State::new(), Box::new(window), (900, 700));
    // The action's own `key:` names its shortcut: its modifiers with its key, and neither half on
    // its own, nor another modifier held with it.
    press(&mut harness, &["command"], "Enter");
    assert_eq!(
        fired(&mut app),
        [ToApp::Fire {
            level: Level::Root,
            key: "archive".to_owned(),
            event: "press".to_owned(),
            value: None,
            gesture: None,
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

/// A window showing `doc` whose bar holds `menu` under its `actions` part, and `content` in it.
fn menu_page(menu: UiNode, content: Vec<UiNode>) -> Vec<ToUi> {
    let mut shell = UiNode::empty("ui.NavShell", "shell");
    let mut actions = UiNode::empty("part", "actions");
    actions
        .props
        .insert("name".to_owned(), Value::text("actions"));
    actions.children.push(menu);
    shell.children.push(actions);
    let mut content_part = UiNode::empty("part", "content");
    content_part
        .props
        .insert("name".to_owned(), Value::text("content"));
    content_part.children = content;
    shell.children.push(content_part);
    vec![ToUi::Open {
        level: Level::Root,
        target: doc(),
        tree: shell,
        front: None,
        capture_protected: false,
    }]
}

/// A `File` menu button, named `key`.
fn file_menu(key: &str) -> UiNode {
    let mut menu = UiNode::empty(&kind("MenuButton"), key);
    menu.props.insert("label".to_owned(), Value::text("File"));
    menu
}

/// A field's own keys are the field's: a plain key a menu item declares types into the field
/// while the keyboard is in it, and the menu's command waits; a chord that types nothing is the
/// window's all the same.
#[test]
fn a_fields_own_keys_edit_its_text_before_a_menus_key_runs() {
    let mut item = UiNode::empty(&kind("MenuItem"), "file#toggle");
    item.props
        .insert("label".to_owned(), Value::text("Toggle the terminal"));
    item.props.insert("key".to_owned(), Value::text("t"));
    item.events
        .insert("choose".to_owned(), wire::Event::default());
    let mut menu = file_menu("file");
    let mut chord = UiNode::empty(&kind("MenuItem"), "file#save");
    chord
        .props
        .insert("label".to_owned(), Value::text("Save as…"));
    chord
        .props
        .insert("key".to_owned(), Value::text("Ctrl+Shift+S"));
    chord
        .events
        .insert("choose".to_owned(), wire::Event::default());
    menu.children.push(item);
    menu.children.push(chord);
    let mut field = UiNode::empty("ui.TextInput", "field");
    field.binding = Some(crate::Binding {
        field: Some(("doc/notes:a".to_owned(), "field".to_owned())),
        kind: Some("notes.Note".to_owned()),
        name: "Field".to_owned(),
    });
    field.props.insert("bind".to_owned(), Value::text("value"));
    let mut page = menu_page(menu, vec![field]);
    page.push(ToUi::Bound {
        level: Level::Root,
        key: "field".to_owned(),
        value: Value::text("Q3 plan"),
        acked: 0,
    });
    let (mut app, mut harness) = drawn(&page);
    harness.click_named("field").expect("the title is drawn");
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_control().as_deref(),
        Some("field"),
        "the keyboard is in the title"
    );
    harness.key("End");
    press(&mut harness, &[], "t");
    assert_eq!(
        shown_value(&harness, "Field").as_deref(),
        Some("Q3 plant"),
        "the plain key typed into the field"
    );
    assert!(
        fired(&mut app).is_empty(),
        "the menu's plain key ran no command while the field holds it"
    );
    press(&mut harness, &["command", "shift"], "S");
    assert_eq!(
        fired(&mut app),
        [ToApp::Fire {
            level: Level::Root,
            key: "file#save".to_owned(),
            event: "choose".to_owned(),
            value: None,
            gesture: None,
        }],
        "a chord that types nothing is the window's, the field in the way or not"
    );
}

/// The keys the window's published surface names for its action `action`, as the palette lists it.
fn palette_keys(window: &AppWindow, action: &str) -> Option<String> {
    fn find<'a>(
        specs: &'a [tilo_actions::ResourceSpec],
        action: &str,
    ) -> Option<&'a tilo_actions::ActionSpec> {
        specs.iter().find_map(|spec| {
            spec.actions
                .iter()
                .find(|offered| offered.id == action)
                .or_else(|| find(&spec.children, action))
        })
    }
    let shown = ui::widget::resources::PublishesResources::resources(
        window,
        &ui::automerge::AutoCommit::new(),
    );
    find(&shown, action)
        .expect("the window publishes the action")
        .state
        .shortcut
        .clone()
}

/// A command the person moved to other keys runs on those, through the control the app declared
/// it on, and no longer on the keys the app gave it; one moved off every key runs on none.
#[test]
fn a_moved_command_runs_on_its_new_keys_and_not_on_the_apps() {
    let (mut app, mut window) = window();
    let mut page = keyed_menu("Ctrl+Enter");
    page.push(ToUi::Actions(catalogue_on(
        "doc",
        "Archive",
        Some("Ctrl+Enter"),
    )));
    send(&mut app, &page);
    window.receive();
    window.set_moved_commands(vec![(
        "new".to_owned(),
        Some("Ctrl+Shift+Enter".to_owned()),
    )]);
    assert_eq!(
        window.command_keys("new").as_deref(),
        Some("Ctrl+Shift+Enter")
    );
    assert_eq!(
        palette_keys(&window, "new").as_deref(),
        Some("Ctrl+Shift+Enter"),
        "the palette names the keys the command is on now"
    );
    let mut harness = ui::headless::Driver::new(ui::State::new(), Box::new(window), (900, 700));
    press(&mut harness, &["command"], "Enter");
    assert!(
        fired(&mut app).is_empty(),
        "the keys the app gave the command are not its any more"
    );
    press(&mut harness, &["command", "shift"], "Enter");
    assert_eq!(
        fired(&mut app),
        [ToApp::Fire {
            level: Level::Root,
            key: "archive".to_owned(),
            event: "press".to_owned(),
            value: None,
            gesture: None,
        }],
        "the new keys run the command through the button the app declared it on"
    );

    drawn_window(&mut harness).set_moved_commands(vec![("new".to_owned(), None)]);
    assert_eq!(
        palette_keys(drawn_window(&mut harness), "new"),
        None,
        "a command on no keys names none"
    );
    for (modifiers, key) in [
        (&["command"][..], "Enter"),
        (&["command", "shift"][..], "Enter"),
    ] {
        press(&mut harness, modifiers, key);
        assert!(
            fired(&mut app).is_empty(),
            "{modifiers:?}+{key}: a command on no keys runs on none"
        );
    }
    drawn_window(&mut harness).set_moved_commands(Vec::new());
    press(&mut harness, &["command"], "Enter");
    assert_eq!(
        fired(&mut app).len(),
        1,
        "put back, it runs on the app's keys"
    );
}

/// The window's primary action is where a person comes to add a list's next item: run from the bar
/// with nothing typed, it puts the keyboard in the list's entry, which declares the action's key,
/// instead of making an item nobody typed, whether it is chosen from the window's catalogue or
/// pressed where it is.
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
    page.push(ToUi::Actions(catalogue_of("New task", Some("Ctrl+n"))));
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

fn press(harness: &mut ui::headless::Driver, modifiers: &[&str], key: &str) {
    let modifiers: Vec<String> = modifiers.iter().map(|name| (*name).to_owned()).collect();
    harness.set_modifiers(&modifiers);
    harness.key(key);
    harness.set_modifiers(&[]);
    drawn_window(harness).flush();
}

fn shown_value(harness: &ui::headless::Driver, name: &str) -> Option<String> {
    let named = node_named(harness, name);
    harness
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == named)
        .and_then(|(_, node)| node.value().map(str::to_owned))
}

/// Two items of a list each declare the key that completes them: the press completes the item the
/// keyboard is on, not the first one the window shows.
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
            level: Level::Root,
            key: format!("{key}#title"),
            value: Value::text(format!("{name} task")),
            acked: 0,
        });
    }
    let mut open = vec![ToUi::Open {
        level: Level::Root,
        target: doc(),
        tree: page,
        front: None,
        capture_protected: false,
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
            level: Level::Root,
            key: "b#complete".to_owned(),
            event: "press".to_owned(),
            value: None,
            gesture: None,
        }],
        "the press completed the item the keyboard is on, not the first one"
    );
}

fn bound_page(kind: &str, props: &[(&str, Value)], value: Value) -> Vec<ToUi> {
    bound_page_to(kind, props, value, "doc/notes:a")
}

/// [`bound_page`] whose field edits `target`: the same editor, the same key, another record.
fn bound_page_to(kind: &str, props: &[(&str, Value)], value: Value, target: &str) -> Vec<ToUi> {
    let mut control = UiNode::empty(kind, "field");
    control.binding = Some(crate::Binding {
        field: Some((target.to_owned(), "field".to_owned())),
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
            level: Level::Root,
            target: doc(),
            tree: page,
            front: None,

            capture_protected: false,
        },
        ToUi::Bound {
            level: Level::Root,
            key: "field".to_owned(),
            value,
            acked: 0,
        },
    ]
}

fn drawn(page: &[ToUi]) -> (local::End, ui::headless::Driver) {
    let (mut app, mut window) = window();
    send(&mut app, page);
    window.receive();
    let mut harness = ui::headless::Driver::new(ui::State::new(), Box::new(window), (1280, 800));
    harness.relayout();
    (app, harness)
}

fn drawn_window(harness: &mut ui::headless::Driver) -> &mut AppWindow {
    harness
        .root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<AppWindow>())
        .expect("the window")
}

fn told(app: &mut local::End) -> Vec<ToApp> {
    sent(app)
        .into_iter()
        .filter(|message| !matches!(message, ToApp::Space { .. }))
        .collect()
}

/// Run `test` with the clock at the day a capture shows (30 June 2026), so a day's fact reads the
/// same on every run, not as "Overdue" once the day has gone by.
fn at_capture_day<T>(test: impl FnOnce() -> T) -> T {
    let at = ui::clock::capture_wall().expect("the capture day exists in the local zone");
    ui::time::testing::with_wall(at, test)
}

fn node_named(harness: &ui::headless::Driver, name: &str) -> ui::accesskit::NodeId {
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
        // What the field is for, and the day the app bound as the day's own fact ("Due, Today"),
        // never as the "Add date" a field that holds no day offers.
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
            Value::some(Value::text(today.strftime("%Y-%m-%d").to_string())),
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

/// A day picked in the calendar reaches the app as the value the field now holds, not as a splice
/// of the text it reads as. A field its app gives no label reads as the day it holds.
#[test]
fn a_picked_day_is_sent_as_the_day_the_field_now_holds() {
    at_capture_day(|| {
        let day = |day: &str| {
            ui::widget::DateField::spoken(
                Some("Due"),
                jiff::civil::Date::strptime("%Y-%m-%d", day).ok(),
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
                level: Level::Root,
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
                level: Level::Root,
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
            level: Level::Root,
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
            level: Level::Root,
            target: doc(),
            key: "field".to_owned(),
            checked: true,
        }],
        "a turn switches it on"
    );
}

fn list_page() -> Vec<ToUi> {
    list_page_keyed("shell")
}

fn list_page_keyed(key: &str) -> Vec<ToUi> {
    let mut list = UiNode::empty("ui.VirtualList", "list");
    list.props.insert("in_column".to_owned(), Value::Bool(true));
    let mut docked = UiNode::empty("ui.Docked", "docked");
    docked.children.push(list);
    let mut overview = UiNode::empty("part", "overview");
    overview
        .props
        .insert("name".to_owned(), Value::text("overview"));
    overview.children.push(docked);
    let mut shell = UiNode::empty("ui.NavShell", key);
    shell.children.push(overview);
    vec![ToUi::Open {
        level: Level::Root,
        target: doc(),
        tree: shell,
        front: None,
        capture_protected: false,
    }]
}

/// A shell with a sidebar and the body `content`, as a notebook draws one.
fn chooser_shell(content: Vec<UiNode>) -> UiNode {
    let part = |name: &str, children: Vec<UiNode>| {
        let mut node = UiNode::empty(PART, &format!("shell#{name}"));
        node.props.insert("name".to_owned(), Value::text(name));
        node.children = children;
        node
    };
    let mut tree = UiNode::empty(&kind("EmptyState"), "shell#none");
    tree.props
        .insert("title".to_owned(), Value::text("No notes yet"));
    let mut shell = UiNode::empty(&kind("NavShell"), "shell");
    shell
        .props
        .insert("sidebar_title".to_owned(), Value::text("Notes"));
    let mut file = UiNode::empty(&kind("MenuButton"), "shell#file");
    file.props.insert("label".to_owned(), Value::text("File"));
    let mut new_note = UiNode::empty(&kind("Button"), "shell#new");
    new_note
        .props
        .insert("label".to_owned(), Value::text("New note"));
    new_note
        .props
        .insert("role".to_owned(), Value::text("primary"));
    new_note.events.insert(
        "press".to_owned(),
        crate::Event {
            invokes: Some(crate::Invocation {
                resource: "1".to_owned(),
                action: "new".to_owned(),
            }),
            ..crate::Event::default()
        },
    );
    shell.children.push(part("sidebar_actions", vec![file]));
    shell.children.push(part("sidebar", vec![tree]));
    shell.children.push(part("content", content));
    shell.children.push(part("actions", vec![new_note]));
    shell
}

fn chooser_page(content: Vec<UiNode>) -> Vec<ToUi> {
    let mut target = doc();
    target.id = "1".to_owned();
    target.ty = "test.tasks".to_owned();
    vec![
        ToUi::Open {
            level: Level::Root,
            target,
            tree: chooser_shell(content),
            front: None,

            capture_protected: false,
        },
        ToUi::Actions(catalogue_of("New note", None)),
    ]
}

/// A one-column window has room for one screen. Where the body holds only an empty state (nothing
/// chosen or open), the screen is the sidebar, where the person chooses; the empty body is no
/// screen to open on.
#[test]
fn a_one_column_window_whose_body_holds_only_an_empty_state_opens_on_its_sidebar() {
    let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
    let mut window = AppWindow::new(Box::new(window_end), Vec::new());
    let mut nothing = UiNode::empty(&kind("EmptyState"), "shell#unopened");
    nothing
        .props
        .insert("title".to_owned(), Value::text("Nothing open"));
    send(&mut app, &chooser_page(vec![nothing.clone()]));
    window.receive();
    let mut harness = ui::headless::Driver::new(ui::State::new(), Box::new(window), (390, 844));
    harness.present_in_window_bar();
    harness.relayout();
    harness.relayout();
    {
        let window = drawn_window(&mut harness);
        let shell = window.shell_ref().expect("the shell");
        assert_eq!(
            shell.geometry().sidebar,
            SidebarPlacement::Screen,
            "a phone shows one column at a time"
        );
        assert_eq!(
            shell.front_pane(),
            NavPane::Sidebar,
            "the window opens on the sidebar, which chooses, not on a body with nothing in it"
        );
    }
    // The list's state carries the action, so the line naming what the control starts stands
    // under its words, where the state does.
    {
        let state = state_in(
            drawn_window(&mut harness).composed().expect("the window"),
            "sidebar",
        );
        assert_eq!(
            state.props.get("action"),
            Some(&Value::text("New note")),
            "the sidebar's state offers the primary action"
        );
        assert_eq!(
            state.props.get("message"),
            Some(&Value::text(crate::i18n::t("tilolang-empty-start-here"))),
            "the line under it says what that control starts"
        );
    }
    // The state says what to do: it offers the app's primary action, and pressing it runs it.
    let action = named_node(&harness, "New note");
    harness
        .perform_a11y_action(action, &ui::a11y::Action::Activate)
        .expect("the state's action presses");
    drawn_window(&mut harness).flush();
    assert!(
        sent(&mut app)
            .iter()
            .any(|message| matches!(message, ToApp::Invoke { .. } | ToApp::Fire { .. })),
        "the state's action reaches the app"
    );
    // A place chosen in the sidebar shows its page in front: the window does not take the person
    // back to the sidebar over a body that has something in it.
    let mut page = UiNode::empty(&kind("Text"), "shell#note");
    page.props.insert("value".to_owned(), Value::text("A note"));
    send(
        &mut app,
        &[ToUi::Open {
            level: Level::Root,
            target: {
                let mut target = doc();
                target.id = "1".to_owned();
                target.ty = "test.tasks".to_owned();
                target
            },
            tree: chooser_shell(vec![page]),
            front: Some(wire::Front::Input),

            capture_protected: false,
        }],
    );
    drawn_window(&mut harness).receive();
    harness.relayout();
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness)
            .shell_ref()
            .expect("the shell")
            .front_pane(),
        NavPane::Content,
        "a body that shows something stays in front of the sidebar"
    );
}

/// The empty state `column` holds in `tree`, where it holds one.
fn state_in(tree: &UiNode, column: &str) -> UiNode {
    fn find(node: &UiNode) -> Option<UiNode> {
        if node.kind == kind("EmptyState") {
            return Some(node.clone());
        }
        node.children.iter().find_map(find)
    }
    crate::shell::part_children(tree, column)
        .iter()
        .find_map(find)
        .unwrap_or_else(|| panic!("{column} holds an empty state: {tree:?}"))
}

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
    // The view beside the list opens before the app has rendered any of the list's rows: the list
    // cannot name the row of the view it shows until they come.
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
            level: Level::Root,
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

fn shell_node(key: &str, props: &[(&str, Value)], parts: Vec<(&str, Vec<UiNode>)>) -> UiNode {
    let mut shell = UiNode::empty(&kind("NavShell"), key);
    for (name, value) in props {
        shell.props.insert((*name).to_owned(), value.clone());
    }
    for (name, children) in parts {
        let mut part = UiNode::empty(PART, &format!("{key}#{name}"));
        part.props.insert("name".to_owned(), Value::text(name));
        part.children = children;
        shell.children.push(part);
    }
    shell
}

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

fn list_node(key: &str) -> UiNode {
    UiNode::empty(&kind("VirtualList"), key)
}

fn bounds_named(harness: &ui::headless::Driver, name: &str) -> ui::kurbo::Rect {
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
/// actions are in the window's one bar from the frame that shows them, where a person presses them,
/// and the list's title is the window's.
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
        level: Level::Root,
        target: doc(),
        tree: root,
        front: None,
        capture_protected: false,
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
            level: Level::Detail,
            target: note("p"),
            tree: project,
            front: None,

            capture_protected: false,
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
fn node_of<'a>(tree: &'a mut UiNode, kind: &str) -> Option<&'a mut UiNode> {
    if tree.kind == kind {
        return Some(tree);
    }
    tree.children
        .iter_mut()
        .find_map(|child| node_of(child, kind))
}

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
        level: Level::Root,
        list: "list".to_owned(),
        len: u32::try_from(keys.len()).expect("a few rows"),
        start: 0,
        keys,
        rows,
    }]
}

/// A window whose own list shows an item as its page is named by the item, as its row names it (a
/// task opened from search reads "Pay rent" in the desktop's tab), and by its page again once the
/// list shows none.
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
            level: Level::Root,
            target: doc(),
            tree: opened,
            front: None,

            capture_protected: false,
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

/// A choice of several rows is the person's own while it holds: the rows the app sends again after
/// a press leave it as it is, and only the window's own putting away of the choice ends it.
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
    let mut chosen = list.selection();
    chosen.select_only(first.clone());
    chosen.toggle(second.clone());
    list.set_selection(chosen);
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

    // What the window kept of the choice gone — an action over the rows ran — the list puts its own
    // away with it.
    send(
        &mut app,
        &[ToUi::Patch {
            level: Level::Root,
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
        "the choice the window put away is put away in the list"
    );
}

/// An entry bound to view state (`ui.TextInput(entry: true, bind: draft)`, Chat's composer) sends
/// what it holds, lets go of it, and takes the very next typing without a click to put the
/// keyboard back: what the person types after a send is the next message, never the sent words
/// with more typed after them. The app's `bound` value for the emptied draft — its own reading of
/// what the send left — never takes the person's new words away, either.
#[test]
fn a_bound_entry_takes_the_next_typing_after_it_sends() {
    use wire::Event;
    let mut page = bound_page(
        "ui.TextInput",
        &[
            ("placeholder", Value::text("Message #general")),
            ("entry", Value::Bool(true)),
        ],
        Value::text("hello"),
    );
    if let Some(ToUi::Open { tree, .. }) = page.first_mut() {
        tree.children[0]
            .events
            .insert("submit".to_owned(), Event::default());
    }
    let (mut app, mut harness) = drawn(&page);
    harness.click_named("field").expect("the composer is drawn");
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_control().as_deref(),
        Some("field"),
        "the click put the keyboard in the composer"
    );
    // Enter submits what the field holds; the entry clears itself and the app hears both.
    harness.key("Enter");
    drawn_window(&mut harness).flush();
    harness.relayout();
    let heard = sent(&mut app);
    assert!(
        heard.iter().any(|message| matches!(
            message,
            ToApp::Fire { key, event, value: Some(Value::Text(text)), .. }
                if key == "field" && event == "submit" && text == "hello"
        )),
        "the send carried the words the field held: {heard:?}"
    );
    assert_eq!(
        drawn_window(&mut harness).focused_control().as_deref(),
        Some("field"),
        "the entry kept the keyboard after it submitted"
    );
    assert_eq!(
        shown_value(&harness, "Field").as_deref(),
        Some(""),
        "the entry let go of what it sent"
    );
    // The app reads its draft back as empty, as the cleared field told it: the field keeps
    // holding nothing, never the sent words pushed back on it.
    send(
        &mut app,
        &[ToUi::Bound {
            level: Level::Root,
            key: "field".to_owned(),
            value: Value::text(""),
            acked: 1,
        }],
    );
    drawn_window(&mut harness).receive();
    harness.relayout();
    // The next words land in the field at once, with no click to bring the keyboard back.
    harness.type_text("and then");
    harness.relayout();
    assert_eq!(
        shown_value(&harness, "Field").as_deref(),
        Some("and then"),
        "the entry took the next typing without being clicked into again"
    );
    // What the person typed after the send is a fresh draft, sent as its own message: the second
    // send carries the new words alone, never the words already sent.
    harness.key("Enter");
    drawn_window(&mut harness).flush();
    let second = sent(&mut app);
    assert!(
        second.iter().any(|message| matches!(
            message,
            ToApp::Fire { key, event, value: Some(Value::Text(text)), .. }
                if key == "field" && event == "submit" && text == "and then"
        )),
        "the second send carried the new words alone: {second:?}"
    );
}

/// A list window: a shell whose overview lists rows, declaring where the window's search goes.
fn searching_list_page(rows: i64) -> Vec<ToUi> {
    let mut list = UiNode::empty("ui.VirtualList", "list");
    list.props.insert("in_column".to_owned(), Value::Bool(true));
    let mut overview = UiNode::empty(PART, "shell#overview");
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
            level: Level::Root,
            target: doc(),
            tree: shell,
            front: None,

            capture_protected: false,
        },
        ToUi::Rows {
            level: Level::Root,
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
    // The app answers with the rows the search left, and the window counts them of the rows the
    // list had before anything was typed.
    send(
        &mut app,
        &[ToUi::RowsChanged {
            level: Level::Root,
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

fn named_node(harness: &ui::headless::Driver, name: &str) -> ui::accesskit::NodeId {
    harness
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("the window draws {name:?}"))
}

/// A list's entry clears once what was typed in it is made: the app answers the submit by setting
/// the bound text back to nothing, while the field still holds its caret at the end of what was
/// typed. The window shows the empty entry, the keyboard still in it, and takes the next item.
#[test]
fn an_entry_the_app_clears_after_its_submit_takes_the_next_item() {
    use wire::Event;
    let mut page = bound_page(
        "ui.TextInput",
        &[
            ("entry", Value::Bool(true)),
            ("placeholder", Value::text("New task for today")),
        ],
        Value::text(""),
    );
    if let Some(ToUi::Open { tree, .. }) = page.first_mut() {
        tree.children[0]
            .events
            .insert("submit".to_owned(), Event::default());
    }
    let (mut app, mut harness) = drawn(&page);
    harness.click_named("field").expect("the entry is drawn");
    harness.type_text("Repot");
    press(&mut harness, &[], "Enter");
    let first = told(&mut app);
    let acked = first
        .iter()
        .filter_map(|message| match message {
            ToApp::Splice { seq, .. } => Some(*seq),
            _ => None,
        })
        .max()
        .unwrap_or_default();
    assert!(
        first.iter().any(|message| matches!(
            message,
            ToApp::Fire { event, value: Some(value), .. }
                if event == "submit" && value == &Value::text("Repot")
        )),
        "the entry submits what was typed: {first:?}"
    );
    // The app made the item and empties the entry, as `add_entry`'s `set(typed, "")` does.
    send(
        &mut app,
        &[
            ToUi::Bound {
                level: Level::Root,
                key: "field".to_owned(),
                value: Value::text(""),
                acked,
            },
            ToUi::Took { fired: 1 },
        ],
    );
    drawn_window(&mut harness).receive();
    harness.relayout();
    assert_eq!(
        shown_value(&harness, "Field").unwrap_or_default(),
        "",
        "the entry is empty again"
    );
    harness.type_text("Sow");
    press(&mut harness, &[], "Enter");
    assert!(
        told(&mut app).iter().any(|message| matches!(
            message,
            ToApp::Fire { event, value: Some(value), .. }
                if event == "submit" && value == &Value::text("Sow")
        )),
        "the cleared entry takes the next item"
    );
}

/// What the app's connection heard at it reaches the window's toolkit once, in the order it was
/// heard; what does not read leaves the window showing the app.
#[test]
fn what_the_app_heard_at_it_reaches_the_toolkit_once() {
    use ui::component_lifecycle::Heard;
    let (mut app, mut window) = window();
    let step = Heard::Interaction(tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::Acting::person("bo"),
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: "create".into(),
            label: "Create".into(),
            app: None,
            step: None,
            creates: None,
            did: None,
        },
        resource: tilo_actions::ActionResource {
            id: "tasks.tracker".into(),
            kind: "tasks.tracker".into(),
            label: "Tasks".into(),
        },
        anchor: tilo_actions::InteractionAnchor::SemanticControl { id: "add".into() },
        group: None,
    });
    let heard = |heard: &Heard| ToUi::Heard(serde_json::to_vec(heard).expect("encodes"));
    send(&mut app, &[heard(&step), ToUi::Heard(b"not json".to_vec())]);
    send(&mut app, &titled_page());
    window.receive();
    assert_eq!(window.take_heard(), vec![step]);
    assert!(window.take_heard().is_empty(), "taken once");
    assert!(
        window.composed().is_some(),
        "the window still shows the app"
    );
}

#[test]
fn a_window_awaits_its_apps_first_view_and_a_part_awaits_nothing() {
    use ui::widget::AwaitsContent as _;
    let (mut app, mut window) = window();
    assert!(
        window.awaits_content(),
        "keys typed before the app's first view wait for it"
    );
    send(&mut app, &[ToUi::Loading]);
    window.receive();
    assert!(
        window.awaits_content(),
        "the loading state is no view of the app's"
    );
    send(&mut app, &titled_page());
    window.receive();
    assert!(!window.awaits_content(), "the view stands: keys go to it");
    let (_app, end) = local::pair();
    let part = AppWindow::part(Box::new(end));
    assert!(
        !part.awaits_content(),
        "a part of a shell holds back none of the shell's keys"
    );
}

/// What an app's run waits on a person for is asked on the system's own permission prompt, naming
/// the action and who asked for it, and the person's choice there is the answer the app hears.
#[test]
fn an_ask_is_the_systems_prompt_and_its_choice_answers_the_app() {
    let mut page = titled_page();
    page.push(ToUi::Ask(Some(wire::Ask {
        action: "Search".to_owned(),
        invoker: "Ada".to_owned(),
    })));
    let (mut app, mut harness) = drawn(&page);
    node_named(&harness, "Allow “Search”? requested by Ada");
    let always = node_named(&harness, "Always allow");
    harness
        .perform_a11y_action(always, &ui::a11y::Action::Activate)
        .expect("the prompt offers to always allow");
    drawn_window(&mut harness).flush();
    let answers: Vec<Answer> = told(&mut app)
        .into_iter()
        .filter_map(|message| match message {
            ToApp::Answer(answer) => Some(answer),
            _ => None,
        })
        .collect();
    assert_eq!(answers, vec![Answer::Always]);
}

/// A field the app marks as where the person types next (a search a press showed) takes the
/// keyboard when it appears, and a field without the mark leaves the keyboard where it is.
#[test]
fn a_field_marked_autofocus_takes_the_keyboard_when_it_appears() {
    let plain = bound_page("ui.TextInput", &[], Value::text(""));
    let (mut app, mut harness) = drawn(&plain);
    assert_eq!(drawn_window(&mut harness).focused_field(), None);
    let search = bound_page(
        "ui.TextInput",
        &[
            ("autofocus", Value::Bool(true)),
            ("placeholder", Value::text("Search YouTube")),
        ],
        Value::text(""),
    );
    send(&mut app, &search);
    drawn_window(&mut harness).receive();
    harness.relayout();
    harness.relayout();
    assert_eq!(
        drawn_window(&mut harness).focused_field().as_deref(),
        Some("field"),
        "the keyboard is in the search field, where the person types next"
    );
}

/// A page in the view that shows the video its engine reports, as a browser's page does: the
/// engine answers what video it shows, and the window's session keeps the answer.
fn page_showing_a_video(rect: ui::kurbo::Rect) -> (ui::widget::WebView, ui::webview::PageHandle) {
    let engine = tilo_webview::test_engine::TestEngine::new();
    engine.set_media_element(Some(tilo_webview::MediaElementInfo {
        element_id: "video:1".to_owned(),
        viewport_rect: [0.1, 0.2, 0.5, 0.3],
        intrinsic_size: [1280, 720],
        label: "A talk on maps".to_owned(),
    }));
    let session: ui::webview::PageHandle = std::rc::Rc::new(std::cell::RefCell::new(Some(
        tilo_page_engine::PageSession::new(engine.clone()),
    )));
    let mut view = ui::widget::WebView::new(engine).with_page(Some(std::rc::Rc::clone(&session)));
    view.place(rect);
    (view, session)
}

/// A player in the view that holds the sound it decoded, as a view's declared player does while a
/// video plays.
#[derive(Default)]
struct SoundingPlayer {
    audio: std::cell::RefCell<Vec<i16>>,
}

impl ui::widget::Widget for SoundingPlayer {
    ui::provides!(dyn ui::widget::AudioTapProvider);

    fn sync(&mut self, _: &ui::widget::AppModel, _: &ui::widget::ModelPath) {}

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _: &ui::Theme,
        _: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> ui::taffy::NodeId {
        use ui::widget::TaffyTreeExt as _;
        taffy.leaf_node(ui::taffy::Style::default())
    }

    fn name(&self) -> &'static str {
        "SoundingPlayer"
    }
}

impl ui::widget::AudioTapProvider for SoundingPlayer {
    fn take_audio(&self) -> Vec<i16> {
        std::mem::take(&mut *self.audio.borrow_mut())
    }
}

/// The sound a player decoded reaches the host that plays it, at whatever depth the view's
/// containers nest it, and once: the host writes it to every place the session's outputs name.
#[test]
fn a_players_sound_reaches_the_host_that_plays_it() {
    let sounding = SoundingPlayer::default();
    sounding.audio.borrow_mut().extend([17_i16, -17, 42, 0]);
    let mut page = AppWindow::new(Box::new(local::pair().1), Vec::new());
    page.body = Box::new(
        Stack::stack()
            .fill_width()
            .fill_height()
            .push_part(part(Box::new(sounding), Fit::fill(1.0))),
    );
    assert_eq!(page.take_audio(), [17_i16, -17, 42, 0]);
    assert!(page.take_audio().is_empty(), "the sound is taken once");
}

/// What a tilolang app says in passing (`notice`) is the window bar's notice: the bar shows its
/// words where its location stands, assistive technology hears it without being interrupted, it
/// leaves by itself after its time or when the app takes it down, and the undo it offers reaches
/// the app as the person's undo.
#[test]
#[expect(clippy::too_many_lines, reason = "one notice's whole life in the bar")]
fn a_notice_the_app_says_shows_in_its_bar_leaves_by_itself_and_its_undo_reaches_the_app() {
    use ui::time::{Duration, testing::with_monotonic_time};
    let words = "Copied work@example.com code";
    let (mut app, mut said_window) = window();
    said_window.receive();
    let mut harness =
        ui::headless::Driver::new(ui::State::new(), Box::new(said_window), (900, 700));
    harness.present_in_window_bar();
    let start = ui::time::now();
    with_monotonic_time(start, || {
        send(
            &mut app,
            &[ToUi::Notice {
                words: Words::Plain(words.to_owned()),
                kind: wire::NoticeKind::Success,
                undo: false,
            }],
        );
        drawn_window(&mut harness).receive();
        harness.relayout();
    });
    assert_eq!(
        ui::window_toolbar::notice().map(|notice| notice.text),
        Some(words.to_owned()),
        "the bar shows what the app said"
    );
    let shown = harness
        .window_bar_notice(900.0)
        .expect("the notice stands in the bar");
    assert_eq!(shown.text, words, "with the app's words whole");
    // Assistive technology hears it, without being interrupted: the bar's notice is a polite
    // live region.
    let heard = harness
        .window_bar_accessibility(900.0)
        .into_iter()
        .find(|node| node.name.as_deref() == Some(words))
        .expect("the bar tells assistive technology what it says");
    assert_eq!(
        heard.live,
        Some(ui::a11y::Politeness::Polite),
        "a passing notice waits for a pause to be read"
    );

    // Its time runs out and the bar lets it go, without the app saying anything more.
    let gone = start + Duration::from_secs(5);
    with_monotonic_time(gone, || harness.relayout());
    assert_eq!(
        ui::window_toolbar::notice(),
        None,
        "a notice leaves by itself after a few seconds"
    );

    // The app takes one down early by saying nothing: a copy that finished is no longer news.
    with_monotonic_time(gone, || {
        send(
            &mut app,
            &[ToUi::Notice {
                words: Words::Plain("Copying the folder…".to_owned()),
                kind: wire::NoticeKind::Info,
                undo: false,
            }],
        );
        drawn_window(&mut harness).receive();
        harness.relayout();
    });
    assert!(
        ui::window_toolbar::notice().is_some(),
        "the app says what is under way"
    );
    with_monotonic_time(gone, || {
        send(
            &mut app,
            &[ToUi::Notice {
                words: Words::Plain(String::new()),
                kind: wire::NoticeKind::Info,
                undo: false,
            }],
        );
        drawn_window(&mut harness).receive();
        harness.relayout();
    });
    assert_eq!(
        ui::window_toolbar::notice(),
        None,
        "and takes it down early by saying nothing"
    );

    // A notice with the undo it offers carries the button beside it, and the press is the app's
    // undo: what the window sends is the same undo the person's Ctrl+Z is.
    let (mut undo_app, mut undo_window) = window();
    undo_window.receive();
    let mut undo_harness =
        ui::headless::Driver::new(ui::State::new(), Box::new(undo_window), (900, 700));
    undo_harness.present_in_window_bar();
    let start = ui::time::now();
    with_monotonic_time(start, || {
        send(
            &mut undo_app,
            &[ToUi::Notice {
                words: Words::Plain("Removed Piranesi".to_owned()),
                kind: wire::NoticeKind::Info,
                undo: true,
            }],
        );
        drawn_window(&mut undo_harness).receive();
        undo_harness.relayout();
    });
    let shown = undo_harness
        .window_bar_notice(900.0)
        .expect("the notice offers the undo");
    assert_eq!(
        shown
            .buttons
            .iter()
            .map(|button| button.name.as_str())
            .collect::<Vec<_>>(),
        ["Undo"],
        "its one button is the undo: {shown:?}"
    );
    undo_harness
        .click_window_notice_action(900.0, 0)
        .expect("the undo takes the press");
    undo_harness.relayout();
    drawn_window(&mut undo_harness).flush();
    assert!(
        sent(&mut undo_app).contains(&ToApp::Undo),
        "pressing it is the app's undo"
    );
}

/// One coded picture of a call, as a device's camera sends it: colour at `width` by `height`,
/// encoded by the real encoder, for the window to decode where it draws.
#[cfg(not(target_arch = "wasm32"))]
fn coded_picture(person: &str, source: wire::CallPictureSource) -> wire::CallPicture {
    let (width, height): (u16, u16) = (32, 32);
    let mut config = media::EncoderConfig::new(media::StreamCodec::H264, 32, 32, 30);
    config.bitrate_bps = 500_000;
    let mut encoder = media::RealtimeEncoder::new(&config).expect("an encoder");
    let rgba = vec![80_u8; 32 * 32 * 4];
    let units = encoder
        .encode(media::InputFrame::Packed(&rgba), std::time::Duration::ZERO)
        .expect("a frame encodes");
    let unit = units.first().expect("one picture");
    let object = ui::native_media::PictureUnit {
        keyframe: unit.keyframe,
        codec: ui::native_media::PictureCodec::H264,
        width,
        height,
        payload: &unit.data,
    }
    .to_object(ui::native_media::PictureLayer::Mid, 1, 0);
    wire::CallPicture {
        person: person.to_owned(),
        stream: format!("{person}/camera"),
        source,
        object,
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_call_picture_is_drawn_where_its_tile_reads_and_forgotten_when_it_stops_showing() {
    let person = "ab".repeat(32);
    let slot = ui::widget::call_slot(&person, CallSource::Camera);
    ui::live_pictures::forget(&slot);
    let (mut app, mut window) = window();
    send(
        &mut app,
        &[ToUi::CallPictures {
            shown: vec![wire::CallShown {
                person: person.clone(),
                source: wire::CallPictureSource::Camera,
            }],
            pictures: vec![coded_picture(&person, wire::CallPictureSource::Camera)],
        }],
    );
    assert!(window.receive());
    let frame = ui::live_pictures::get(&slot).expect("the tile reads the frame it shows");
    assert_eq!((frame.width, frame.height), (32, 32));

    send(
        &mut app,
        &[ToUi::CallPictures {
            shown: Vec::new(),
            pictures: Vec::new(),
        }],
    );
    assert!(window.receive());
    assert!(
        ui::live_pictures::get(&slot).is_none(),
        "a tile nobody shows keeps no frame"
    );
}

/// A view the app declared `protect capture` for: the window draws it as a secret's, so every
/// capture leaves it out, and opening a view that declares nothing again makes it capturable.
#[test]
fn a_window_showing_a_protected_view_is_withheld_from_captures() {
    let open = |protected| {
        vec![ToUi::Open {
            level: Level::Root,
            target: doc(),
            tree: UiNode::empty("stack", "page"),
            front: None,
            capture_protected: protected,
        }]
    };
    let (_app, harness) = drawn(&open(true));
    assert!(
        harness.capture_protected(),
        "the window keeps what it shows"
    );

    let (_app, harness) = drawn(&open(false));
    assert!(!harness.capture_protected());

    // A detail view that declares it protects the window over what the root view shows.
    let mut page = open(false);
    page.push(ToUi::Open {
        level: Level::Detail,
        target: doc(),
        tree: UiNode::empty("stack", "detail"),
        front: None,
        capture_protected: true,
    });
    let (_app, harness) = drawn(&page);
    assert!(
        harness.capture_protected(),
        "one view of the window is enough"
    );
}

/// The shell stands a window for the picture a view floats, and the page's video moving in the
/// app's own window is told once: the surface mirrors where the video stands, follows it when it
/// scrolls, and stands still when nothing moved.
#[test]
fn a_floating_picture_is_drawn_where_the_shell_stands_it_and_follows_its_source() {
    use ui::floating_surface::FloatingSurfaceWidgetExt as _;
    let rect = ui::kurbo::Rect::new(0.0, 100.0, 1_000.0, 600.0);
    let (mut video, _session) = page_showing_a_video(rect);
    video.set_pip(true);
    let mut page = AppWindow::new(Box::new(local::pair().1), Vec::new());
    page.set_app_id("so.tilo.browser");
    page.body = Box::new(
        Stack::stack()
            .fill_width()
            .fill_height()
            .push_part(part(Box::new(video), Fit::fill(1.0))),
    );
    let _ = page.take_floating_surface_request();
    let _ = page.take_floating_surface_request();

    let id = "so.tilo.browser:picture-in-picture";
    let window = ui::kurbo::Rect::new(600.0, 120.0, 960.0, 322.5);
    let content = ui::kurbo::Rect::new(608.0, 128.0, 952.0, 314.5);
    page.on_floating_surface_host_message(&FloatingSurfaceHostMessage::PrepareSurface {
        id: id.to_owned(),
        token: "t0k3n".to_owned(),
        rect: ui::floating_surface::protocol_rect_array(window),
        content_rect: ui::floating_surface::protocol_rect_array(content),
        transfer_covered: false,
        coordinate_offset: [0.0, 0.0],
    });
    let presentations = page.floating_surface_presentations();
    assert_eq!(presentations.len(), 1, "the picture is the one surface");
    assert_eq!(presentations[0].id, id);
    assert_eq!(presentations[0].window_rect, window);
    assert_eq!(presentations[0].content_rect, content);

    // Where the video stands is told once, and again only when the page moved it.
    assert_eq!(
        page.take_floating_surface_source_updates(),
        vec![(
            id.to_owned(),
            ui::kurbo::Rect::new(100.0, 200.0, 600.0, 350.0)
        )]
    );
    assert!(
        page.take_floating_surface_source_updates().is_empty(),
        "a picture that stands where it did is not told again"
    );
    assert!(
        page.floating_surface_moves_on_drag(id),
        "the person drags the picture where they want it"
    );
    assert!(
        !page.floating_surface_moves_on_drag("so.tilo.browser:call"),
        "a card with controls of its own is not dragged by its body"
    );

    // The shell returns the picture: nothing of it is left to draw or to move.
    page.on_floating_surface_host_message(&FloatingSurfaceHostMessage::Closed {
        id: id.to_owned(),
    });
    assert!(page.floating_surface_presentations().is_empty());
    assert!(page.take_floating_surface_source_updates().is_empty());
}

/// The page the card is drawn on, as its view's root: a stack that holds it.
fn page_of(children: Vec<UiNode>) -> Vec<ToUi> {
    let mut page = UiNode::empty("stack", "page");
    page.children = children;
    vec![ToUi::Open {
        level: Level::Root,
        target: doc(),
        tree: page,
        front: None,

        capture_protected: false,
    }]
}

/// A layout a view gave a `ui.ContextMenu`: a right-click anywhere in the layout opens the menu at
/// the pointer, and the row chosen runs the action the view bound to it, as a row's own menu does,
/// from the same shared area. The menu takes no place of its own in the layout.
#[test]
fn a_right_click_on_a_layout_opens_the_menu_its_view_placed() {
    use wire::Event;
    let mut choose = UiNode::empty("ui.MenuItem", "new");
    choose
        .props
        .insert("label".to_owned(), Value::text("New workspace"));
    choose.events.insert("choose".to_owned(), Event::default());
    let mut menu = UiNode::empty("ui.ContextMenu", "menu");
    menu.children.push(choose);
    let mut held = UiNode::empty("stack", "held");
    held.props.insert("fill".to_owned(), Value::Float(1.0));
    let mut layout = UiNode::empty("layers", "layers");
    layout.props.insert("fill".to_owned(), Value::Float(1.0));
    layout.children = vec![menu, held];
    let (mut app, mut harness) = drawn(&page_of(vec![layout]));
    assert!(
        !harness
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label().is_some_and(|label| label == "New workspace")),
        "the menu stands closed until it is opened"
    );
    harness.right_click_at(640.0, 400.0);
    harness.relayout();
    let row = node_named(&harness, "New workspace");
    let bounds = harness
        .a11y_node(row)
        .and_then(|node| node.bounds())
        .expect("the row stands at the pointer");
    assert!(
        bounds.x0 >= 400.0 && bounds.y0 >= 200.0 && bounds.x1 <= 1000.0 && bounds.y1 <= 800.0,
        "the menu opens where the right-click landed: {bounds:?}"
    );
    let (x, y) = (
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    harness.click_at(x, y);
    drawn_window(&mut harness).flush();
    assert!(
        matches!(&fired(&mut app)[..], [ToApp::Fire { key, event, .. }] if key == "new" && event == "choose"),
        "choosing the row runs the action the view bound to it: {:?}",
        sent(&mut app)
    );
}
