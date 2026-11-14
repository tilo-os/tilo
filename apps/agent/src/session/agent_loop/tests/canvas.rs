//! An agent asked on the canvas (`docs/designs/app-language/shell.md` §8.1): its first turn
//! leads with the canvas' own steps and what there is to show, never with the opens of every host
//! program the computer has installed; it answers in words and shows what answers it beside the
//! conversation, within a few turns; and it arranges and restyles the canvas with the session's
//! own steps.

use super::*;

/// The session's own surface: the arrangement, two places standing, and the person's viewing
/// record whose `show-style` switches the style.
const SESSION_PEER: u64 = 31;
/// The desktop's opens of the host programs installed on this computer.
const HOST_PEER: u64 = 32;
/// The tilolang apps' records the session serves: Tasks and Mail.
const RECORDS_PEER: u64 = 33;
/// Notes, which publishes its open.
const NOTES_PEER: u64 = 34;
/// The session's door for a task's programs.
const DOOR_PEER: u64 = 35;
/// The Tasks app's root, as the session serves it.
const TASKS: &str = "tl:so.tilo.lang.tasks:root-t";
/// Mail's root.
const MAIL: &str = "tl:so.tilo.lang.mail:root-m";
/// The place showing the inbox.
const INBOX_PLACE: &str = "shell:place:3";
/// The place showing a note.
const NOTE_PLACE: &str = "shell:place:4";

fn on(
    resource: &str,
    kind: &str,
    label: &str,
    action: &str,
    action_label: &str,
    params: Vec<ActionParam>,
) -> ExportedAction {
    ExportedAction {
        resource: resource.into(),
        resource_kind: kind.into(),
        resource_label: label.into(),
        ..descriptor(
            action,
            action_label,
            tilo_actions::OperationKind::Effect,
            params,
            Availability::Available,
            vec![],
        )
    }
}

fn surface(peer: u64, app: &str, actions: Vec<ExportedAction>) -> BrokerActionSurface {
    BrokerActionSurface {
        peer_id: peer,
        provider: None,
        app_id: app.into(),
        cluster_id: Uuid::from_u128(1).to_string(),
        record_id: String::new(),
        live: true,
        actions,
    }
}

/// The session's canvas, the tilolang apps' records, a setting of the session's that is no
/// arranging, and the opens of host programs.
pub(super) fn surfaces() -> Vec<BrokerActionSurface> {
    // Notes' own open, by its title: a request naming Notes asks for something of it.
    let notes = vec![on(
        "app.so.tilo.lang.notes",
        crate::surface::APPLICATION_KIND,
        "Notes",
        "open",
        "Notes",
        vec![],
    )];
    // The session's door for the task's programs, offered to the agent for its runs: no app's
    // window, and so no app's own host, is open.
    let door = vec![on(
        tilo_actions::remote::task_programs::RESOURCE,
        tilo_actions::remote::task_programs::RESOURCE,
        "Task programs",
        tilo_actions::remote::task_programs::RUN,
        "Run a step of a task",
        vec![
            ActionParam::required("program", ParamType::Text, "program"),
            ActionParam::optional("confirm", ParamType::Boolean, "confirm"),
            ActionParam::optional("this", ParamType::Text, "this"),
            ActionParam::required("run", ParamType::Text, "run"),
            ActionParam::optional("label", ParamType::Text, "label"),
        ],
    )];
    vec![
        surface(SESSION_PEER, "so.tilo.session", session_actions()),
        surface(RECORDS_PEER, "so.tilo.session", record_actions()),
        surface(NOTES_PEER, "so.tilo.lang.notes", notes),
        surface(HOST_PEER, "so.tilo.desktop", host_actions()),
        surface(DOOR_PEER, "so.tilo.agent", door),
    ]
}

/// The session's own actions: the arrangement's, two places' steps, the viewing record's
/// `show-style`, the chrome's `open-app` and a network setting.
fn session_actions() -> Vec<ExportedAction> {
    let text = |name: &'static str| ActionParam::optional(name, ParamType::Text, name);
    let int = |name: &'static str| ActionParam::optional(name, ParamType::Integer, name);
    let arrangement = |action: &str, label: &str, params| {
        on(
            "shell:layout",
            "shell",
            "The person's arrangement",
            action,
            label,
            params,
        )
    };
    let mut session = vec![
        arrangement(
            "place",
            "Show a resource on the canvas, as one of its views",
            vec![
                ActionParam::required("resource", ParamType::Resource, "The resource it shows"),
                text("view"),
                int("x"),
                int("y"),
                text("beside"),
            ],
        ),
        arrangement(
            "open",
            "Open a resource with its own view, beside what the ask is in",
            vec![
                ActionParam::required("resource", ParamType::Resource, "The resource it shows"),
                text("view"),
                text("beside"),
            ],
        ),
        on(
            "tl:so.tilo.shell:root-v",
            "tl.so.tilo.shell.Viewing",
            "so.tilo.shell:frames",
            "show-style",
            "Show as…",
            vec![ActionParam::required(
                "shown",
                ParamType::Text,
                "shown: Text",
            )],
        ),
        on(
            "desktop:chrome",
            "desktop.chrome",
            "The desktop",
            "open-app",
            "Open an app",
            vec![ActionParam::required("app", ParamType::Text, "The app")],
        ),
        on(
            "settings:settings.network",
            "settings.network",
            "Network",
            "set-wifi",
            "Turn Wi-Fi on or off",
            vec![ActionParam::required("on", ParamType::Boolean, "on")],
        ),
    ];
    session.extend(place_steps(INBOX_PLACE, "Inbox — Mail"));
    session.extend(place_steps(NOTE_PLACE, "Groceries — Notes"));
    session
}

/// The steps of the place `place`: its close, its position and its activate.
fn place_steps(place: &str, label: &str) -> Vec<ExportedAction> {
    let text = |name: &'static str| ActionParam::optional(name, ParamType::Text, name);
    let int = |name: &'static str| ActionParam::optional(name, ParamType::Integer, name);
    vec![
        on(
            place,
            "shell.place",
            label,
            "close",
            "Close a place",
            vec![],
        ),
        on(
            place,
            "shell.place",
            label,
            "position",
            "Stand a place at a point on the canvas",
            vec![int("x"), int("y"), text("beside")],
        ),
        on(
            place,
            "shell.place",
            label,
            "activate",
            "Bring a place in front",
            vec![],
        ),
    ]
}

/// The tilolang apps' records the session serves: the Tasks root and one task, and Mail.
fn record_actions() -> Vec<ExportedAction> {
    vec![
        on(
            TASKS,
            "tl.so.tilo.lang.tasks.Tasks",
            "Tasks",
            "open",
            "Open",
            vec![],
        ),
        on(
            &format!("{TASKS}/i1"),
            "tl.so.tilo.lang.tasks.Task",
            "Pay rent",
            "open",
            "Open",
            vec![],
        ),
        on(
            MAIL,
            "tl.so.tilo.lang.mail.Mail",
            "Mail",
            "open",
            "Open",
            vec![],
        ),
    ]
}

/// The desktop's opens of the host programs installed on this computer.
fn host_actions() -> Vec<ExportedAction> {
    ["Alacritty", "org.gnome.Calculator", "chromium-browser"]
        .into_iter()
        .map(|program| {
            on(
                &format!("{}host:{program}", tools::LAUNCHER_OPEN_PREFIX),
                "desktop.application",
                program,
                "open",
                &format!("Open {program}"),
                vec![],
            )
        })
        .collect()
}

/// The id the model is shown for `action` on `resource` of the session.
fn session_op(peer: u64, resource: &str, action: &str) -> String {
    format!("broker:{peer}:{resource}#{action}")
}

fn canvas_invoke(
    peer: u64,
    resource: &str,
    action: &str,
    arguments: &Value,
    reply: Option<&str>,
) -> (&'static str, Value) {
    let mut input = serde_json::json!({
        "operation": session_op(peer, resource, action),
        "parameters": arguments,
    });
    if let Some(reply) = reply {
        input[tools::INVOKE_REPLY] = reply.into();
    }
    (tools::INVOKE, input)
}

fn tilo(program: &str) -> (&'static str, Value) {
    (
        crate::programs::TILO,
        serde_json::json!({ "program": program }),
    )
}

/// The steps the session ran, by action, with their arguments.
fn ran(world: &World, action: &str) -> Vec<serde_json::Map<String, Value>> {
    world
        .steps
        .iter()
        .filter(|(seen, _)| seen == action)
        .map(|(_, arguments)| arguments.clone())
        .collect()
}

/// A run asked `title` in a conversation on the canvas: the workspace runs tilolang apps, and the
/// session publishes its canvas beside them.
fn canvas_fixture(title: &str, turns: Vec<Vec<TurnEvent>>) -> Fixture {
    let mut f = fixture(title, canvas_world(), turns);
    let mut published = canvas_world().surfaces();
    published.extend(surfaces());
    f.session
        .apply_broker_catalogue(&ui::collaboration::CatalogueChange::snapshot(&published));
    f
}

fn canvas_world() -> World {
    World {
        tilolang: false,
        tl_programs: vec![(
            "due",
            serde_json::json!({
                "text": "[i1 \"Pay rent\" due: 2026-11-17, i2 \"Call Ana\" due: 2026-11-19]",
                "failed": false,
                "answer": {"state": "done", "output": [], "steps": [], "changed": 0, "apps": []},
            }),
        )],
        ..World::default()
    }
}

/// The first turn of a canvas conversation leads with the canvas: how to show a resource beside
/// the conversation, the arranging and style steps, the places standing and what there is to
/// show. The opens of host programs are not in it at all, and the session's settings are not
/// among the canvas' steps.
#[test]
fn the_first_turn_leads_with_the_canvas_and_leaves_host_programs_out() {
    let mut f = canvas_fixture(
        "which of my tasks are due this week?",
        vec![turn(&[finish("Nothing is due.", true)], 64)],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap();
    let first = bodies[0].to_string();
    let offered = tool_names(&bodies[0]);
    drop(bodies);
    let canvas_at = first.find("The canvas: where the person sees things.");
    let applications_at = first.find("Applications:");
    assert!(
        canvas_at.is_some() && applications_at.is_some() && canvas_at < applications_at,
        "the canvas comes before the applications: {first}"
    );
    assert!(
        first.contains("This conversation stands on it, as `chat.workspace/channels/loop`"),
        "{first}"
    );
    // The request body is JSON: the call's own quotes stand escaped in it.
    assert!(
        first.contains(&format!(
            "Show a resource: `invoke` with `\\\"operation\\\": \\\"{}\\\"` and \
             `\\\"parameters\\\": {{\\\"resource\\\": …}}`",
            session_op(SESSION_PEER, "shell:layout", "place")
        )),
        "{first}"
    );
    for (resource, action) in [
        ("shell:layout", "open"),
        ("tl:so.tilo.shell:root-v", "show-style"),
        (INBOX_PLACE, "close"),
        (NOTE_PLACE, "position"),
    ] {
        assert!(
            first.contains(&session_op(SESSION_PEER, resource, action)),
            "the canvas lists {resource}#{action}: {first}"
        );
    }
    assert!(first.contains("Inbox — Mail — shell:place:3"), "{first}");
    assert!(
        first.contains(&format!(
            "{TASKS} — Tasks (tl.so.tilo.lang.tasks.Tasks); 1 items"
        )),
        "what there is to show names Tasks' root and counts its items: {first}"
    );
    assert!(first.contains(&format!("{MAIL} — Mail")), "{first}");
    // No app's window is open, yet programs reach every app the session serves through its door.
    assert!(
        first.contains("- `tasks`: its record is a `tasks.Tasks`"),
        "the apps a program can use, open or not: {first}"
    );
    assert!(
        offered.contains(&crate::programs::TILO.to_owned()),
        "the `tilo` tool is offered"
    );
    for host in ["Alacritty", "org.gnome.Calculator", "chromium-browser"] {
        assert!(
            !first.contains(host),
            "no host program's open: {host} in {first}"
        );
    }
    // The canvas' steps are not summarised a second time among the applications'.
    let applications = &first[applications_at.unwrap()..];
    assert!(
        !applications.contains("shell:layout#place"),
        "{applications}"
    );
}

/// "Which of my tasks are due this week?" asked on the canvas: one program reads them, and the
/// next turn places Tasks beside the conversation with the answer in words — two model calls,
/// no app opened in a window, and the run succeeds with the words.
#[test]
fn a_question_is_answered_in_words_and_its_view_placed_beside_the_conversation() {
    let reply = "Two tasks are due this week: Pay rent on Tuesday and Call Ana on Thursday.";
    let mut f = canvas_fixture(
        "which of my tasks are due this week?",
        vec![
            turn(
                &[tilo(
                    "find(tasks.Task).filter(t -> t.due <= date(\"2026-11-22\"))",
                )],
                64,
            ),
            turn(
                &[canvas_invoke(
                    SESSION_PEER,
                    "shell:layout",
                    "place",
                    &serde_json::json!({"resource": TASKS}),
                    Some(reply),
                )],
                64,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 2, "two model calls");
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: reply.into()
        }
    );
    let world = f.world.borrow();
    let placed = ran(&world, "place");
    assert_eq!(placed.len(), 1, "{:?}", world.steps);
    assert_eq!(placed[0]["resource"], TASKS);
    assert!(
        !placed[0].contains_key("x") && !placed[0].contains_key("y"),
        "no point: it stands beside the conversation"
    );
    assert!(
        ran(&world, "open").is_empty() && ran(&world, "open-app").is_empty(),
        "no app opened: {:?}",
        world.steps
    );
}

/// "Show me my mails" places the inbox in one call; "put my notes next to the inbox" stands the
/// note's place beside the inbox's; "switch to strips" shows the shell as strips. Each is one
/// model call, and a request naming an app (Notes) is satisfied by arranging that app's place.
#[test]
fn showing_arranging_and_restyling_are_one_step_each() {
    let cases: [(&str, (&'static str, Value), &str); 3] = [
        (
            "show me my mails",
            canvas_invoke(
                SESSION_PEER,
                "shell:layout",
                "place",
                &serde_json::json!({"resource": MAIL}),
                Some("Your inbox is beside us."),
            ),
            "place",
        ),
        (
            "put my notes next to the inbox",
            canvas_invoke(
                SESSION_PEER,
                NOTE_PLACE,
                "position",
                &serde_json::json!({"beside": INBOX_PLACE}),
                Some("Your notes stand next to the inbox."),
            ),
            "position",
        ),
        (
            "switch to strips",
            canvas_invoke(
                SESSION_PEER,
                "tl:so.tilo.shell:root-v",
                "show-style",
                &serde_json::json!({"shown": "so.tilo.shell:strips"}),
                Some("The shell shows strips now."),
            ),
            "show-style",
        ),
    ];
    for (asked, call, action) in cases {
        let mut f = canvas_fixture(asked, vec![turn(&[call], 64)]);
        f.session.run_task(&f.run).unwrap();
        assert_eq!(
            f.script.bodies.lock().unwrap().len(),
            1,
            "{asked}: one model call"
        );
        assert!(
            matches!(
                terminal(&f.sent.borrow()),
                runtime_collab::TaskRunUpdate::Succeeded { .. }
            ),
            "{asked}: {:?}",
            terminal(&f.sent.borrow())
        );
        assert_eq!(ran(&f.world.borrow(), action).len(), 1, "{asked}");
    }
}
