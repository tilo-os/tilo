#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::json;
use tasks::model::{ModelChoice, Status, issue_resource};
use tasks::{App, Tracker};
use tilo_agent_models::{OfferedModel, OfferedModels};
use ui::prelude::*;

fn open() -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<App>::for_person("ada").expect("open")
}

fn only(app: &TestApp<App>) -> Id {
    app.state().issues.ids().next().cloned().expect("an issue")
}

#[test]
fn an_issue_made_through_its_action_is_listed_opened_and_kept() {
    let mut app = open();
    assert!(app.state().issues.is_empty(), "a tracker starts empty");
    app.call("create", json!({ "title": "Write the launch notes" }))
        .expect("create");
    let issue = only(&app);
    assert_eq!(app.state().issues.get(&issue).unwrap().number, 1);
    assert!(app.harness().targets().contains_key("tasks-list"));

    // Opening it, as a search hit or an agent does, shows its page.
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");
    assert!(app.harness().targets().contains_key("tasks-issue-title"));

    // Its description is a rich-text resource of its own beneath it.
    let body = format!("{}/body", issue_resource(&issue));
    app.call_on(&body, "append-paragraph", json!({ "text": "Say why" }))
        .expect("write the description");
    assert!(
        app.state()
            .issues
            .get(&issue)
            .unwrap()
            .body
            .text()
            .contains("Say why")
    );
    assert!(app.undo());
    assert!(
        !app.state()
            .issues
            .get(&issue)
            .unwrap()
            .body
            .text()
            .contains("Say why")
    );
}

#[test]
fn placing_on_the_board_and_commenting_are_the_issues_actions() {
    let mut app = open();
    app.call("create", json!({ "title": "Record the demo" }))
        .expect("create");
    let issue = only(&app);
    app.call_on(
        &issue_resource(&issue),
        "place",
        json!({ "status": "in-progress" }),
    )
    .expect("place");
    assert_eq!(
        app.state().issues.get(&issue).unwrap().status,
        Status::InProgress
    );
    app.call_on(
        &issue_resource(&issue),
        "comment",
        json!({ "text": "Two minutes at most" }),
    )
    .expect("comment");
    let state: Tracker = app.state();
    let thread = &state.issues.get(&issue).unwrap().comments;
    assert_eq!(thread.values().next().unwrap().author, "ada");
}

#[test]
fn a_viewer_reads_the_tracker_but_cannot_change_it() {
    let mut app = open();
    app.call("create", json!({ "title": "Pick the venue" }))
        .expect("create");
    app.set_role(ui::Role::Viewer);
    assert!(
        app.call("create", json!({ "title": "Another" })).is_err(),
        "a viewer creates nothing"
    );
    assert_eq!(app.state().issues.len(), 1);
}

/// `#2` is the tracker's to resolve: its `open` shows the issue that number
/// names, a number no issue has is refused, and the page shows what the
/// issue links to as chips that open it where it lives.
#[test]
fn a_reference_as_it_reads_opens_its_issue_whose_links_are_chips() {
    let mut app = open();
    app.call("create", json!({ "title": "First" }))
        .expect("create");
    app.call("create", json!({ "title": "Second" }))
        .expect("create");
    let (second, _) = app
        .state()
        .by_number(2)
        .map(|(id, issue)| (id.clone(), issue.clone()))
        .expect("numbered 2");
    app.call_on(
        &issue_resource(&second),
        "link",
        json!({
            "app": "so.tilo.notes",
            "kind": "notes.page",
            "resource": "notes.page:1",
            "label": "Plan",
        }),
    )
    .expect("link the issue to a page");

    app.call("open", json!({ "anchor": "#2" }))
        .expect("open the issue numbered 2");
    assert!(app.harness().targets().contains_key("tasks-issue-title"));
    assert!(
        app.harness().a11y_node_named("#2").is_some(),
        "its page, not #1's"
    );
    assert!(app.harness().a11y_node_named("#1").is_none());
    assert!(
        app.harness().a11y_node_named("Open Plan").is_some(),
        "the link reads as what it opens"
    );
    assert!(app.call("open", json!({ "anchor": "#9" })).is_err());
}

/// "Link a note" takes a note already written as the reference the system
/// palette picks by its title (and an agent passes): it publishes what it
/// picks, so nobody types an id, and links the note it names, never
/// something else.
#[test]
fn a_note_picked_by_its_title_is_linked_as_it_reads() {
    let mut app = open();
    app.call("create", json!({ "title": "Plan the launch" }))
        .expect("create");
    let issue = only(&app);
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");
    let picked = app
        .actions()
        .expect("the actions")
        .into_iter()
        .find(|action| {
            action.address.resource == issue_resource(&issue)
                && action.address.action == "link-note"
        })
        .and_then(|action| action.params.into_iter().find(|p| p.name == "note"))
        .and_then(|param| param.pick());
    assert_eq!(
        picked,
        Some(ui::tilo_actions::Pick::Resource {
            kind: "notes.notebook.pages".into()
        }),
        "the note is picked, never typed"
    );
    let note = json!({
        "app": tasks::NOTES,
        "kind": "notes.notebook.pages",
        "id": "notes.notebook/pages/7",
        "label": "Launch checklist",
    });
    app.call_on(
        &issue_resource(&issue),
        "link-note",
        json!({ "note": note }),
    )
    .expect("link the note");
    let links: Vec<_> = app
        .state()
        .issues
        .get(&issue)
        .unwrap()
        .links
        .values()
        .cloned()
        .collect();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].app, tasks::NOTES);
    assert_eq!(links[0].kind, "notes.notebook.pages");
    assert_eq!(links[0].resource, "notes.notebook/pages/7");
    assert_eq!(links[0].label, "Launch checklist");
    let issue_ref = json!({
        "app": "so.tilo.tasks",
        "kind": "tasks.tracker.issues",
        "id": "tasks.tracker/issues/1",
        "label": "Another issue",
    });
    assert!(
        app.call_on(
            &issue_resource(&issue),
            "link-note",
            json!({ "note": issue_ref })
        )
        .is_err(),
        "only a note"
    );
}

/// A link names the record of the app that keeps what it links to, and
/// opens it there; a link made before links named a record opens the app's
/// own record of the workspace.
#[test]
fn a_link_opens_in_the_record_it_names() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let room = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";
    env.set("TILO_WORKSPACE_ID", room);
    let door = std::sync::Arc::new(ui::tilo_actions::remote::TestHost::new());
    let _served = ui::RemoteActions::enter(
        door.clone(),
        ui::tilo_actions::remote::Caller::local_person("ada"),
    );
    let mut app = open();
    app.call("create", json!({ "title": "Plan the launch" }))
        .expect("create");
    let issue = only(&app);
    for (label, record) in [("Kept", Some("/launch")), ("Old", None)] {
        let mut link = json!({
            "app": "so.tilo.tasks",
            "kind": "tasks.tracker.issues",
            "resource": format!("tasks.tracker/issues/{label}"),
            "label": label,
        });
        if let Some(record) = record {
            link["record"] = json!(record);
        }
        app.call_on(&issue_resource(&issue), "link", link)
            .expect("link the issue");
    }
    let links: Vec<_> = app
        .state()
        .issues
        .get(&issue)
        .unwrap()
        .links
        .values()
        .cloned()
        .collect();
    assert_eq!(links[0].record.as_deref(), Some("/launch"));
    assert_eq!(links[1].record, None);
    // What a link stored before it named a record reads as naming none.
    let old = <tasks::model::Link as ui::tilo_resource::Model>::from_json(&json!({
        "label": "Old", "app": "so.tilo.notes", "kind": "notes.page", "resource": "notes.page:1",
    }))
    .expect("an old link reads");
    assert_eq!(old.record, None);

    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");
    for (label, record) in [("Kept", Some("/launch")), ("Old", None)] {
        let chip = app
            .harness()
            .a11y_node_named(&format!("Open {label}"))
            .expect("the link's chip");
        app.harness()
            .perform_a11y_action(chip, &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        let asked = door.take_asked().pop().expect("one open");
        let reference = ui::tilo_reference::ResourceRef::whole(
            "tasks.tracker.issues",
            format!("tasks.tracker/issues/{label}"),
            label,
        );
        assert_eq!(
            asked.target,
            reference.open_target(room, "so.tilo.tasks", record)
        );
    }
}

/// Settings, which keeps the models the person's providers offer for an
/// agent's run: Tasks reads its record without being able to change it.
struct SettingsOwner;

impl ResourceApp for SettingsOwner {
    type Resource = OfferedModels;
    const ID: &'static str = "so.tilo.settings";
    const TITLE: &'static str = "Settings fixture";

    fn view(_: &mut ViewCx<OfferedModels>) -> Element {
        Box::new(Text::new("Settings fixture"))
    }
}

/// The issue's model is picked among those the person's providers offer,
/// read from their record of them; picking the agent's own clears it.
#[test]
fn an_issues_model_is_picked_among_the_offered_ones() {
    let mut app = open();
    app.call("create", json!({ "title": "Summarise the notes" }))
        .expect("create");
    let issue = only(&app);
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");

    let mut settings = TestApp::<SettingsOwner>::for_person("ada").expect("settings");
    let offered = settings.shared_person::<OfferedModels>().expect("kept");
    offered
        .invoke(&OfferedModels::OFFER.with(vec![OfferedModel {
            profile: "local".into(),
            provider: "Local".into(),
            model: "fast-1".into(),
            label: "Fast".into(),
        }]))
        .expect("offer");
    let read = app.shared_person::<OfferedModels>().expect("read");
    read.record()
        .merge_peer_document(offered.record().document().expect("document"))
        .expect("merge");
    app.settle();

    let model = |app: &TestApp<App>| {
        app.state()
            .issues
            .get(&issue)
            .and_then(|issue| issue.model.clone())
    };
    app.harness()
        .perform_named(
            "tasks-issue-model",
            &ui::a11y::Action::SetValue("Fast (Local)".into()),
        )
        .expect("pick the offered model");
    app.settle();
    assert_eq!(
        model(&app),
        Some(ModelChoice {
            profile: "local".into(),
            model: "fast-1".into(),
        })
    );
    app.harness()
        .perform_named(
            "tasks-issue-model",
            &ui::a11y::Action::SetValue("The agent's own model".into()),
        )
        .expect("pick the agent's own");
    app.settle();
    assert_eq!(model(&app), None);
}

/// The assignee is picked among the workspace's people and agents by name,
/// never typed as an id; Nobody clears it.
#[test]
fn an_issues_assignee_is_picked_among_the_rooms_people_and_agents() {
    let mut app = open();
    app.call("create", json!({ "title": "Draft the agenda" }))
        .expect("create");
    let issue = only(&app);
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");
    {
        let presence = app.harness().presence_mut();
        presence.set_display_name(ui::permissions::UserId("bob".into()), "Bob");
        presence.set_display_name(ui::permissions::UserId("agent:scribe".into()), "Scribe");
        presence.set_participant_kind(
            ui::permissions::UserId("agent:scribe".into()),
            ui::presence::ParticipantKind::Agent,
        );
    }
    app.settle();

    let assignee = |app: &TestApp<App>| {
        app.state()
            .issues
            .get(&issue)
            .and_then(|issue| issue.assignee.clone())
            .map(|assignee| assignee.0)
    };
    let pick = |app: &mut TestApp<App>, name: &str| {
        app.harness()
            .perform_named(
                "tasks-issue-assignee",
                &ui::a11y::Action::SetValue(name.into()),
            )
            .unwrap_or_else(|error| panic!("pick {name}: {error:?}"));
        app.settle();
    };
    pick(&mut app, "Bob");
    assert_eq!(assignee(&app).as_deref(), Some("person:bob"));
    pick(&mut app, "Scribe");
    assert_eq!(assignee(&app).as_deref(), Some("agent:scribe"));
    pick(&mut app, "Nobody");
    assert_eq!(assignee(&app), None);
}

/// What a run asks is answered on the host's sheet: the issue's Answer
/// button sends the tracker's answer through the session's door, and the
/// app records no answer of its own.
#[test]
fn a_runs_question_is_answered_through_the_host() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    let door = std::sync::Arc::new(ui::tilo_actions::remote::TestHost::new());
    let _served = ui::RemoteActions::enter(
        door.clone(),
        ui::tilo_actions::remote::Caller::local_person("ada"),
    );
    let mut app = open();
    app.call(
        "create",
        json!({ "title": "Send the notes", "assignee": "agent:scribe" }),
    )
    .expect("create");
    let issue = only(&app);
    app.call("approve-run", json!({ "issue": issue.as_str() }))
        .expect("start the run");
    let agent = ActionInvoker::actor("agent:scribe")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    app.call_by(
        agent,
        "tasks.tracker",
        "ask-confirmation",
        json!({
            "issue": issue.as_str(),
            "question": {
                "id": "q1", "app": "so.tilo.mail", "action": "send",
                "resource": "mail.account", "arguments_digest": "digest",
                "draft": { "action_label": "Send message", "resource_label": "Mail",
                           "fields": [] },
            },
        }),
    )
    .expect("the run asks");
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");
    let answer = app
        .harness()
        .a11y_node_named("Answer")
        .expect("the answer button");
    app.harness()
        .perform_a11y_action(answer, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    let asked = door.take_asked().pop().expect("one answer");
    assert_eq!(
        asked.target.provider,
        ui::tilo_actions::remote::OWN_PROVIDER
    );
    assert_eq!(asked.target.address.action, "answer-confirmation");
    let ui::tilo_actions::remote::InvokeArgument::Json(arguments) = asked.argument else {
        panic!("typed arguments");
    };
    assert_eq!(arguments["issue"], json!(issue.as_str()));
    let run = app.state().issues.get(&issue).unwrap().run.clone().unwrap();
    assert_eq!(
        run.confirmation.unwrap().state,
        tasks::model::ConfirmationState::Waiting,
        "only the host answers"
    );
}

/// Each semantics node of an inspection, outer and nested, with the rect it
/// stands for: its own bounds, else the area of what it is drawn in, as the
/// browser's accessibility mirror places it.
fn semantic_nodes(snapshot: &serde_json::Value) -> Vec<(serde_json::Value, [f64; 4])> {
    fn rect(value: &serde_json::Value) -> Option<[f64; 4]> {
        let r = value.as_array()?;
        Some([
            r[0].as_f64()?,
            r[1].as_f64()?,
            r[2].as_f64()?,
            r[3].as_f64()?,
        ])
    }
    fn bounds(node: &serde_json::Value, enclosing: [f64; 4]) -> [f64; 4] {
        let b = &node["bounds"];
        match (
            b["x0"].as_f64(),
            b["y0"].as_f64(),
            b["x1"].as_f64(),
            b["y1"].as_f64(),
        ) {
            (Some(x0), Some(y0), Some(x1), Some(y1)) => [x0, y0, x1 - x0, y1 - y0],
            _ => enclosing,
        }
    }
    fn nested(
        node: &serde_json::Value,
        enclosing: [f64; 4],
        out: &mut Vec<(serde_json::Value, [f64; 4])>,
    ) {
        let at = bounds(node, enclosing);
        out.push((node.clone(), at));
        for child in node["children"].as_array().into_iter().flatten() {
            nested(child, at, out);
        }
    }
    fn walk(layout: &serde_json::Value, out: &mut Vec<(serde_json::Value, [f64; 4])>) {
        if layout["semantics"].is_object() {
            nested(
                &layout["semantics"],
                rect(&layout["rect"]).unwrap_or_default(),
                out,
            );
        }
        for child in layout["children"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&snapshot["root"], &mut out);
    out
}

#[test]
fn the_sidebar_names_what_it_narrows_and_the_bar_names_the_place() {
    let mut app = open();
    app.harness().present_in_window_bar();
    let snapshot: serde_json::Value =
        serde_json::from_str(&app.harness().inspection_json().expect("inspection")).expect("json");
    let nodes = semantic_nodes(&snapshot);
    let drawn = |at: &[f64; 4]| at[2] > 0.0 && at[3] > 0.0;

    // The sidebar is named for what it narrows, apart from the app's title
    // the heading and the window's tab carry, and keeps one identity.
    let trees: Vec<_> = nodes.iter().filter(|(n, _)| n["role"] == "Tree").collect();
    assert_eq!(trees.len(), 1, "one tree: {trees:?}");
    let (tree, at) = trees[0];
    assert_eq!(tree["name"], "Filters");
    assert_eq!(tree["semantic_key"], "tasks.filters");
    assert!(drawn(at), "the tree is drawn: {at:?}");

    // Where the window is, the bar says: the place, since no issue is open,
    // and nothing in the content titles it a second time.
    let bar = ui::window_toolbar::declared().expect("the window declares its slots");
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some("Issues")
    );
    let headings: Vec<_> = nodes
        .iter()
        .filter(|(n, _)| n["role"].get("Heading").is_some())
        .collect();
    assert!(
        !headings
            .iter()
            .any(|(n, at)| n["name"] == "Issues" && drawn(at)),
        "the place is the bar's location, not a heading of the list's: {headings:?}"
    );
}

/// The sidebar narrows the list to what a person is looking for — their own
/// issues, the ones nobody took, an agent's, and a label's — and the board
/// follows the same choice.
#[test]
fn the_sidebar_narrows_the_list_and_the_board_to_a_filter() {
    let mut app = open();
    for (title, assignee, label) in [
        ("Write the launch notes", Some("person:ada"), Some("launch")),
        ("Record the demo", None, None),
        ("Tidy the onboarding docs", Some("agent:claude"), None),
    ] {
        app.call("create", json!({ "title": title, "assignee": assignee }))
            .expect("create");
        if let Some(label) = label {
            let issue = app
                .state()
                .issues
                .iter()
                .find(|(_, issue)| issue.title == title)
                .map(|(id, _)| id.clone())
                .expect("the issue just made");
            app.call("add-label", json!({ "issue": issue, "label": label }))
                .expect("label it");
        }
    }
    let rows = |app: &mut TestApp<App>| -> Vec<String> {
        let snapshot: serde_json::Value =
            serde_json::from_str(&app.harness().inspection_json().expect("inspection"))
                .expect("json");
        let nodes = semantic_nodes(&snapshot);
        nodes
            .iter()
            .filter_map(|(node, _)| {
                // The list's rows read as references (`#2 Record the demo`);
                // the sidebar's rows are its filters, which have no number.
                let name = node["name"].as_str().unwrap_or_default();
                (name.starts_with('#')).then(|| name.to_owned())
            })
            .collect()
    };

    // Everything, to begin with.
    assert_eq!(rows(&mut app).len(), 3, "every issue shows");

    // Mine: the person reading, whose issues are their own.
    choose(&mut app, "Mine");
    let shown = rows(&mut app);
    assert_eq!(shown.len(), 1, "and nothing assigned to anyone else");
    assert!(
        shown
            .iter()
            .any(|row| row.contains("Write the launch notes")),
        "my own issue shows: {shown:?}"
    );

    // Nobody's: the issue without an assignee.
    choose(&mut app, "Nobody's");
    let shown = rows(&mut app);
    assert_eq!(shown.len(), 1, "one issue is unassigned: {shown:?}");
    assert!(shown[0].contains("Record the demo"), "{shown:?}");

    // Todos: the tasks nobody has started. All three are as they were
    // made, so all three are todos, an agent's included until its run
    // takes the task up.
    choose(&mut app, "Todos");
    assert_eq!(rows(&mut app).len(), 3, "nothing has been started");

    // An agent's: the one handed to the room's agent.
    choose(&mut app, "An agent's");
    let shown = rows(&mut app);
    assert_eq!(shown.len(), 1, "one issue is an agent's: {shown:?}");
    assert!(shown[0].contains("Tidy the onboarding docs"), "{shown:?}");

    // A label: the issues carrying it.
    choose(&mut app, "launch");
    let shown = rows(&mut app);
    assert_eq!(shown.len(), 1, "one issue carries the label: {shown:?}");

    // The board follows the same choice: the agent's issue is the only card
    // in a lane.
    choose(&mut app, "An agent's");
    press_view(&mut app, "Board");
    let harness = app.harness();
    assert!(
        harness
            .a11y_node_named("#3 Tidy the onboarding docs")
            .is_some(),
        "the agent's issue is a card on the board"
    );
    assert!(
        harness.a11y_node_named("#2 Record the demo").is_none(),
        "and nobody else's issue is drawn beside it"
    );

    // Back to the list and everything in it.
    press_view(&mut app, "Issues");
    choose(&mut app, "All issues");
    assert_eq!(rows(&mut app).len(), 3, "every issue shows again");
}

/// One task, read through every view the app has: the conversation it is
/// is the thread, and the card on the board and the row in the list open the
/// same page rather than a copy of it.
#[test]
fn a_task_with_a_conversation_reads_in_the_thread_and_opens_its_own_page() {
    let mut app = open();
    app.call("create", json!({ "title": "Which venue seats 120?" }))
        .expect("create");
    let issue = only(&app);
    app.call_on(
        &issue_resource(&issue),
        "comment",
        json!({ "text": "The old depot on Mill Street does." }),
    )
    .expect("say something on it");

    // The thread reads it as a conversation, saying who last spoke.
    press_view(&mut app, "Thread");
    app.settle();
    let row = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label().is_some_and(|label| {
                label.contains("#1") && label.contains("Which venue seats 120?")
            })
        })
        .map(|(id, _)| id)
        .expect("the task reads in the thread");
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();

    // Its row opens the issue's page, the same one the other views open.
    assert!(
        app.harness().targets().contains_key("tasks-issue-title"),
        "the thread's row opens the issue"
    );
    assert!(
        app.harness()
            .a11y_node_named("The old depot on Mill Street does.")
            .is_some(),
        "and the page it opens is the issue that was read in the thread"
    );
}

/// Press the window bar's view `name`, as a press on it does.
fn press_view(app: &mut TestApp<App>, name: &str) {
    let harness = app.harness();
    let bar = ui::window_toolbar::declared().expect("the window declares its views");
    let views = bar.views.expect("the window declares its views");
    let view = views
        .choices
        .iter()
        .find(|view| view.label == name)
        .expect("the bar offers the view");
    harness.press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
        id: view.id.clone(),
    });
    let _ = harness.poll_frame();
}

/// Choose the sidebar's `name` row, as a press on it does.
fn choose(app: &mut TestApp<App>, name: &str) {
    let harness = app.harness();
    let node = harness
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("the sidebar offers {name:?}"));
    harness
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("{name} is chosen: {error}"));
    let _ = harness.poll_frame();
}

/// Notes as the session's door reaches it: makes the note each call asks
/// for and answers its resource, as `create-page` does.
fn notes_door() -> ui::tilo_actions::remote::TestHost {
    let made = std::sync::atomic::AtomicUsize::new(0);
    ui::tilo_actions::remote::TestHost::new().completing(move |_| {
        let count = made
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        let note = format!("notes.notebook/pages/{count}");
        ui::tilo_actions::ActionOutput {
            value: Some(json!({ "resource": note })),
            resources: vec![note],
        }
    })
}

/// Writing an issue's note asks Notes through its own published action, as
/// whoever asked: an agent's invocation is continued, so the host decides
/// the note as the agent's. The note Notes made is linked once, as theirs,
/// and their undo takes the link back. Pressed here, it is the app's own
/// request.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: the agent's note, its undo, then the person's press"
)]
fn an_issues_note_is_made_by_notes_for_whoever_asked_and_linked_once() {
    use ui::tilo_actions::{ActionInvoker, InvocationUpdate};
    let mut env = tilo_test_env::ScopedEnv::new();
    let room = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";
    env.set("TILO_WORKSPACE_ID", room);
    let door = std::sync::Arc::new(notes_door());
    let _served = ui::RemoteActions::enter(
        door.clone(),
        ui::tilo_actions::remote::Caller::local_person("ada"),
    );
    let mut app = open();
    app.call("create", json!({ "title": "Plan the launch" }))
        .expect("create");
    let issue = only(&app);
    let before = app.versions().len();

    let scribe = ActionInvoker::actor("agent:scribe");
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: app.harness().component_instance_id().unwrap(),
        address: ui::tilo_resource::Effect::address_on(
            &tasks::TasksEffect::WriteNote {
                issue: issue.clone(),
            },
            "tasks.tracker",
        ),
    };
    let (invocation, _) = app
        .harness()
        .start_bound_job_for(
            Some(scribe.clone()),
            &address,
            json!({ "issue": issue }).as_object().unwrap(),
            true,
        )
        .expect("the agent's invocation starts");
    app.settle();

    let asked = door.asked();
    assert_eq!(asked.len(), 1, "one call of Notes: {asked:?}");
    let notebook = ui::tilo_reference::ResourceRef::whole("notes.notebook", "notes.notebook", "");
    assert_eq!(
        asked[0].target,
        notebook.action_target(room, tasks::NOTES, None, "create-page")
    );
    assert_eq!(
        asked[0].argument,
        ui::tilo_actions::remote::InvokeArgument::Json(
            json!({ "title": "#1 Plan the launch" })
                .as_object()
                .unwrap()
                .clone()
        )
    );
    assert_eq!(
        asked[0].on_behalf_of,
        Some(invocation),
        "the call continues the agent's invocation"
    );
    assert!(
        app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| matches!(
                update,
                InvocationUpdate::Succeeded { output, .. }
                    if output.value == Some(json!({ "resource": "notes.notebook/pages/1" }))
            )),
        "{:?}",
        app.harness().job_updates(invocation)
    );

    let links: Vec<_> = app
        .state()
        .issues
        .get(&issue)
        .unwrap()
        .links
        .values()
        .cloned()
        .collect();
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0].app, tasks::NOTES);
    assert_eq!(links[0].kind, "notes.notebook.pages");
    assert_eq!(links[0].resource, "notes.notebook/pages/1");
    assert_eq!(links[0].label, "#1 Plan the launch");
    assert_eq!(links[0].record, None);

    let versions = app.versions();
    assert_eq!(versions.len(), before + 1, "one step: {versions:?}");
    let by = versions
        .last()
        .and_then(|version| version.context.as_ref())
        .and_then(|context| context.invoker.as_ref())
        .expect("the step names who made it");
    assert_eq!(by.actor, "agent:scribe");

    app.call_by(scribe, "tasks.tracker", "undo", json!({}))
        .expect("the agent undoes its step");
    assert!(app.state().issues.get(&issue).unwrap().links.is_empty());

    // The person pressing it in the window: no invocation of anyone else's
    // is behind it, so it is the app's own request of Notes.
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");
    let button = app
        .harness()
        .a11y_node_named("Write a note")
        .expect("the issue offers to write its note");
    app.harness()
        .perform_a11y_action(button, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    let asked = door.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[1].on_behalf_of, None);
    assert_eq!(
        app.state().issues.get(&issue).unwrap().links.len(),
        1,
        "the note linked as the person's"
    );
}

/// The window bar's sidebar button flips the sidebar on every click, in a
/// window wide enough to dock it and in a narrow one.
#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar() {
    ui::framework::testing::check_window_sidebar_button(|| {
        ui::framework::testing::TestApp::<App>::for_person("ada")
    })
    .unwrap_or_else(|error| panic!("{error}"));
}

/// A project is a resource of its own above the tasks: it is made,
/// listed in the sidebar, opened by the address it is published under, and
/// holds the tasks that work towards it and the people involved.
#[test]
fn a_project_is_made_listed_and_opened_by_its_address() {
    let mut app = open();
    assert!(app.state().areas.is_empty(), "a tracker starts empty");

    // The project page's own action makes one, with the kind it is given.
    app.call("create-area", json!({ "name": "Launch", "kind": "area" }))
        .expect("create the area");
    let (area, made) = app
        .state()
        .areas
        .iter()
        .next()
        .map(|(id, area)| (id.clone(), area.clone()))
        .expect("the area just made");
    assert_eq!(made.name, "Launch");
    assert_eq!(made.kind, tasks::model::Kind::Area);

    // Its address opens its page, as a row of the sidebar or a search hit
    // does, and the page offers what it holds.
    let address = tasks::model::area_resource(&area);
    app.call_on(&address, "open", json!({}))
        .expect("open the project");
    assert!(app.harness().targets().contains_key("tasks-project-name"));
    assert!(
        app.harness()
            .targets()
            .contains_key("tasks-project-involve")
    );
    let bar = ui::window_toolbar::declared().expect("the window declares its slots");
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some("Launch"),
        "the bar says which project the window shows"
    );

    // Someone involved is part of the project, in their own words.
    app.call_on(
        &address,
        "involve",
        json!({ "name": "Grace", "role": "Writes the launch notes" }),
    )
    .expect("involve Grace");
    let people = app.state().area(&area).expect("the project").people.clone();
    assert_eq!(people.len(), 1);
    assert_eq!(people.iter().next().expect("Grace").1.name, "Grace");

    // Undo takes the involvement back: it is an ordinary change, kept as
    // any other, and the project stays.
    assert!(app.undo());
    assert!(
        app.state()
            .area(&area)
            .expect("the project still stands")
            .people
            .is_empty()
    );
}

/// The task list gathers what a person follows, and one chip says how a
/// run is doing wherever the issue is read: on its page, on the board and in
/// the list. Pressing the chip opens the issue through the app's own open
/// effect, which is also what a script and an agent invoke, so following a
/// task from the list lands in the one task view.
#[test]
fn a_running_task_reads_the_same_in_the_list_and_on_its_issue() {
    let mut app = open();
    app.call(
        "create",
        json!({ "title": "Send the notes", "assignee": "agent:scribe" }),
    )
    .expect("create");
    let issue = only(&app);
    let follow = |app: &TestApp<App>| {
        app.state()
            .issues
            .get(&issue)
            .expect("the issue")
            .run
            .clone()
    };

    // A task no run has taken up is not one the list follows.
    assert!(follow(&app).is_none(), "no run was started yet");

    app.call("approve-run", json!({ "issue": issue.as_str() }))
        .expect("start the run");
    assert!(follow(&app).is_some(), "the run is on the issue");

    // The list is reached through the filter the sidebar publishes.
    choose(&mut app, "Running tasks");

    // The row carries the task's state as the chip the issue and the board
    // show, and the chip says what the run is doing.
    let chip = format!("tasks-run-chip-{issue}");
    assert!(
        app.harness().targets().contains_key(&chip),
        "the list shows the task's state: {:?}",
        app.harness().targets().keys().collect::<Vec<_>>()
    );

    // Pressing it opens the issue: the one task view, not a second one.
    app.harness()
        .click_named(&chip)
        .expect("press the task's state");
    let _ = app.harness().poll_frame();
    assert!(
        app.harness().targets().contains_key("tasks-issue-title"),
        "the chip opens the issue"
    );

    // Its lifecycle actions are the issue's own, offered to a person and to
    // a script alike, so the list needs no second copy of them.
    let pause = app
        .state()
        .issues
        .get(&issue)
        .expect("the issue")
        .run
        .is_some();
    assert!(pause);
    app.call("pause-run", json!({ "issue": issue.as_str() }))
        .expect("pause the run");
    assert!(
        app.state()
            .issues
            .get(&issue)
            .expect("the issue")
            .run
            .as_ref()
            .is_some_and(|run| run.paused.is_some()),
        "a paused run stays the task, held"
    );
}

/// Whether the list's New issue button can be pressed now.
fn new_issue_enabled(app: &mut TestApp<App>) -> bool {
    let harness = app.harness();
    let button = harness
        .a11y_node_named("New issue")
        .expect("the list offers New issue");
    let (_, node) = harness
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == button)
        .expect("the button is in the tree");
    !node.is_disabled()
}

/// The row that makes an issue cannot make one without a title: the view
/// can only hand `create` a title it parsed, so the button is disabled and
/// Enter does nothing while the field is blank, and nothing is refused or
/// shown over the window. Once the field says something, Enter and the
/// button each make an issue and empty the field.
#[test]
fn a_blank_new_issue_cannot_be_invoked() {
    let mut app = open();
    app.settle();
    assert!(!new_issue_enabled(&mut app), "a blank title disables it");
    app.harness()
        .click_named("tasks-list-new-create")
        .expect("press New issue");
    app.settle();
    app.harness()
        .click_named("tasks-list-new-title")
        .expect("focus the title");
    app.harness().type_text("   ");
    app.harness().key("Enter");
    app.settle();
    assert!(app.state().issues.is_empty(), "nothing blank is made");
    assert!(
        app.refused().is_none(),
        "nothing was refused: {:?}",
        app.refused()
    );
    assert!(
        app.harness().refusal_said().is_none(),
        "nothing is said over the window"
    );
    assert!(!new_issue_enabled(&mut app), "spaces are still blank");

    app.harness().type_text("Ship it");
    app.settle();
    assert!(new_issue_enabled(&mut app), "a title enables it");
    app.harness().key("Enter");
    app.settle();
    let issue = only(&app);
    assert_eq!(app.state().issues.get(&issue).unwrap().title, "Ship it");
    assert!(
        !new_issue_enabled(&mut app),
        "the field is emptied for the next"
    );

    app.harness().type_text("Write the notes");
    app.settle();
    app.harness()
        .click_named("tasks-list-new-create")
        .expect("press New issue");
    app.settle();
    let titles: Vec<String> = app
        .state()
        .issues
        .values()
        .map(|issue| issue.title.clone())
        .collect();
    assert_eq!(titles, ["Ship it", "Write the notes"]);
}

/// An issue opens as a chat in a window of its own: the published
/// `open-chat` asks the desktop for another window of Tasks beside this one,
/// launched on the issue's chat, and that window shows the conversation
/// alone. The run's steps arrive in it, what the run asks is answered
/// through the host, and a message after the run ended starts the next run
/// with it.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: the chat opened, the run's steps, its question, its end and what comes after"
)]
fn an_issue_opens_as_a_chat_of_its_own_where_the_agent_is_told_more() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    let door = std::sync::Arc::new(ui::tilo_actions::remote::TestHost::new());
    let _served = ui::RemoteActions::enter(
        door.clone(),
        ui::tilo_actions::remote::Caller::local_person("ada"),
    );
    let mut app = open();
    app.call(
        "create",
        json!({ "title": "Review the applications", "assignee": "agent:scribe" }),
    )
    .expect("create");
    let issue = only(&app);
    app.call(
        "tell-agent",
        json!({ "issue": issue.as_str(), "text": "Start with the newest" }),
    )
    .expect("telling the agent starts its run");

    let _ = ui::shell_tabs::take_requests();
    app.effect(&tasks::TasksEffect::OpenChat {
        issue: issue.clone(),
    })
    .expect("open the issue as a chat");
    assert_eq!(
        ui::shell_tabs::take_requests(),
        [ui::shell_tabs::TabRequest::OpenBeside {
            target: Some(tasks::chat_uri(&issue)),
            background: false,
        }],
        "the chat is another window of Tasks, launched on the issue's chat"
    );
    assert!(
        app.effect(&tasks::TasksEffect::OpenChat {
            issue: Id::from_raw("gone"),
        })
        .is_err(),
        "an issue that is not there opens no chat"
    );

    let mut chat = app
        .relaunch(&[&tasks::chat_uri(&issue)])
        .expect("the window launched on the chat");
    chat.settle();
    let targets = chat.harness().targets();
    for shown in ["tasks-chat-draft", "tasks-chat-send", "stop-run"] {
        assert!(
            targets.contains_key(shown),
            "{shown} is in the chat: {:?}",
            targets.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        !targets.contains_key("tasks-list") && !targets.contains_key("tasks.filters"),
        "a chat window shows the conversation alone"
    );

    // The run's steps arrive as the agent posts them; it asks first.
    let agent = || {
        ActionInvoker::actor("agent:scribe")
            .with_kind(ActorKind::Agent)
            .acting_for("ada")
    };
    chat.call_by(
        agent(),
        "tasks.tracker",
        "report-run",
        json!({ "issue": issue.as_str(), "phase": "executing",
                "text": "1 of 2: reviewed Jane Doe" }),
    )
    .expect("the run reports a step");
    chat.call_by(
        agent(),
        "tasks.tracker",
        "ask-confirmation",
        json!({
            "issue": issue.as_str(),
            "question": {
                "id": "q1", "app": "so.tilo.mail", "action": "send",
                "resource": "mail.account", "arguments_digest": "digest",
                "draft": { "action_label": "Send message", "resource_label": "Mail",
                           "fields": [] },
            },
        }),
    )
    .expect("the run asks");
    chat.settle();
    assert!(chat.harness().targets().contains_key("tasks-chat-question"));
    chat.harness()
        .click_named("tasks-run-answer")
        .expect("press Answer");
    chat.settle();
    let asked = door
        .take_asked()
        .pop()
        .expect("the answer goes to the host");
    assert_eq!(asked.target.address.action, "answer-confirmation");
    assert_eq!(
        chat.state()
            .issues
            .get(&issue)
            .unwrap()
            .run
            .as_ref()
            .unwrap()
            .phase,
        tasks::model::RunPhase::Waiting,
        "only the host answers"
    );

    // The run ends; the conversation goes on: the next message starts the
    // next run, which reads it as its direction.
    chat.call_by(
        agent(),
        "tasks.tracker",
        "answer-confirmation",
        json!({ "issue": issue.as_str(), "confirm": false, "reason": "Not yet" }),
    )
    .expect_err("an agent never answers for its person");
    chat.call(
        "answer-confirmation",
        json!({ "issue": issue.as_str(), "confirm": false, "reason": "Not yet" }),
    )
    .expect("the person declines");
    chat.call_by(
        agent(),
        "tasks.tracker",
        "finish-run",
        json!({ "issue": issue.as_str(), "outcome": "done",
                    "summary": "Reviewed both" }),
    )
    .expect("the run finishes");
    chat.settle();
    assert!(
        !chat.harness().targets().contains_key("stop-run"),
        "a run that ended offers no Stop: {:?}",
        chat.harness().targets().keys().collect::<Vec<_>>()
    );
    chat.harness()
        .click_named("tasks-chat-draft")
        .expect("focus the field");
    chat.harness().type_text("Now draft the replies");
    chat.settle();
    chat.harness()
        .click_named("tasks-chat-send")
        .expect("press Send");
    chat.settle();
    let state = chat.state();
    let told = state.issues.get(&issue).unwrap();
    let run = told.run.as_ref().unwrap();
    assert_eq!((run.epoch, run.phase.active()), (2, true));
    let directions: Vec<&str> = told
        .messages_for_run(2)
        .map(|(_, message)| message.text.as_str())
        .collect();
    assert_eq!(directions, ["Now draft the replies"]);
}

/// An issue is document state: an agent reads what it holds through the
/// issue's own document read, as `tilo_read` on it does, and the issue is
/// disclosed by that read rather than hidden behind its `open`.
#[test]
fn an_agent_reads_an_issue_through_its_document_read() {
    let mut app = open();
    app.call(
        "create",
        json!({ "title": "Review Ada's portfolio", "body": "Look at her GitHub." }),
    )
    .expect("create");
    app.settle();
    let issue = issue_resource(&only(&app));
    let content = app
        .call_on(&issue, "read", json!({}))
        .expect("the issue offers its document read")
        .expect("the read answers");
    assert_eq!(content["title"], "Review Ada's portfolio", "{content}");
    assert_eq!(
        ui::tilo_actions::hidden_from_agents(&app.harness().resources()),
        [],
        "nothing of the tracker is kept from an agent"
    );
}
