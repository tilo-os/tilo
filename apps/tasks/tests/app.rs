#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::json;
use tasks::model::{Status, issue_resource};
use tasks::{App, Tracker};
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
    let workspace = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";
    env.set("TILO_WORKSPACE_ID", workspace);
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
            reference.open_target(workspace, "so.tilo.tasks", record)
        );
    }
}

/// The assignee is picked among the workspace's people and agents by name,
/// never typed as an id, and reads back as the person's name and face; the
/// "Nobody" choice clears it.
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
    let pick = |app: &mut TestApp<App>, entry: &str| {
        app.harness()
            .perform_named("tasks-issue-assignee", &ui::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("open the assignee: {error:?}"));
        app.settle();
        app.harness()
            .perform_named(entry, &ui::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("pick {entry}: {error:?}"));
        app.settle();
    };
    pick(&mut app, "tasks-issue-assignee-bob");
    assert_eq!(assignee(&app).as_deref(), Some("person:bob"));
    assert!(
        app.harness().a11y_node_named("Assignee: Bob").is_some(),
        "the chip says the person's name, never the record's spelling"
    );
    pick(&mut app, "tasks-issue-assignee-agent:scribe");
    assert_eq!(assignee(&app).as_deref(), Some("agent:scribe"));
    assert!(
        app.harness()
            .a11y_node_named("Assignee: Scribe · Agent")
            .is_some(),
        "an agent reads by name, and as an agent"
    );
    pick(&mut app, "tasks-issue-assignee-none");
    assert_eq!(assignee(&app), None);
    assert!(
        app.harness().a11y_node_named("Assignee: Nobody").is_some(),
        "clearing says nobody is doing it"
    );
}

/// An issue's labels are the chips it carries and one `+` that opens the
/// labels the workspace already uses and the field that makes a new one.
#[test]
fn an_issues_labels_are_added_from_one_chip() {
    let mut app = open();
    app.call("create", json!({ "title": "Draft the agenda" }))
        .expect("create");
    let issue = only(&app);
    // An issue already labelled makes that label one the picker offers.
    app.call("create", json!({ "title": "Book the room" }))
        .expect("create");
    let other = app
        .state()
        .issues
        .iter()
        .find(|(_, shown)| shown.title == "Book the room")
        .map(|(id, _)| id.clone())
        .expect("the other issue");
    app.call("add-label", json!({ "issue": other, "label": "launch" }))
        .expect("label it");
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");
    app.settle();

    // One control in the row, not three: the chips and the `+`; the field
    // waits in what the `+` opens.
    assert!(
        app.harness()
            .targets()
            .contains_key("tasks-issue-add-label"),
        "the labels end in one chip"
    );
    assert!(
        !app.harness()
            .targets()
            .contains_key("tasks-issue-label-new"),
        "no field stands in the row"
    );

    let labels_of = |app: &TestApp<App>| -> Vec<String> {
        app.state()
            .issues
            .get(&issue)
            .map(|shown| shown.labels.iter().cloned().collect())
            .unwrap_or_default()
    };

    // The `+` offers the label the workspace already uses.
    app.harness()
        .perform_named("tasks-issue-add-label", &ui::a11y::Action::Activate)
        .expect("open the labels");
    app.settle();
    app.harness()
        .perform_named("tasks-issue-label-launch", &ui::a11y::Action::Activate)
        .expect("pick launch");
    app.settle();
    assert_eq!(labels_of(&app), ["launch"], "the label lands on the issue");

    // Typing a new one in its field makes that one too.
    app.harness()
        .perform_named("tasks-issue-add-label", &ui::a11y::Action::Activate)
        .expect("open the labels");
    app.settle();
    app.harness()
        .click_named("tasks-issue-label-new")
        .expect("the field takes the click");
    app.harness().type_text("venue");
    app.settle();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        labels_of(&app),
        ["launch", "venue"],
        "a typed label is made on the issue"
    );
}

/// The fields stay one row at any width: they wrap rather than leave the
/// window or cover each other, and what the `+` opens fits in the window
/// too.
#[test]
fn the_issue_fields_wrap_instead_of_leaving_the_window() {
    for (width, height) in [(1240_u32, 780_u32), (520, 700)] {
        let mut app =
            TestApp::<App>::capture_at(Some("issue"), (width, height)).expect("the capture opens");
        app.settle();
        let fields = [
            "tasks-issue-status",
            "tasks-issue-assignee",
            "tasks-issue-add-label",
        ];
        let targets = app.harness().targets();
        let rects: Vec<(&str, ui::kurbo::Rect)> = fields
            .iter()
            .map(|id| {
                let rect = targets
                    .get(*id)
                    .unwrap_or_else(|| panic!("{id} is laid out at {width}: {:?}", targets.keys()));
                (*id, *rect)
            })
            .collect();
        for (id, rect) in &rects {
            assert!(
                rect.x0 >= 0.0
                    && rect.y0 >= 0.0
                    && rect.x1 <= f64::from(width)
                    && rect.y1 <= f64::from(height),
                "{id} stays in the {width} wide window: {rect:?}"
            );
        }
        for (at, (one, a)) in rects.iter().enumerate() {
            for (other, b) in &rects[at + 1..] {
                assert!(
                    a.x1 <= b.x0 || b.x1 <= a.x0 || a.y1 <= b.y0 || b.y1 <= a.y0,
                    "{one} and {other} do not cover each other at {width}: {a:?} {b:?}"
                );
            }
        }
        app.harness()
            .perform_named("tasks-issue-add-label", &ui::a11y::Action::Activate)
            .expect("open the labels");
        app.settle();
        let targets = app.harness().targets();
        let mut opened = Vec::new();
        for id in ["tasks-issue-label-new", "tasks-issue-label-create"] {
            let rect = targets
                .get(id)
                .unwrap_or_else(|| panic!("{id} is laid out at {width}: {:?}", targets.keys()));
            assert!(
                rect.x0 >= 0.0
                    && rect.y0 >= 0.0
                    && rect.x1 <= f64::from(width)
                    && rect.y1 <= f64::from(height),
                "{id} fits the {width} wide window: {rect:?}"
            );
            opened.push((id, *rect));
        }
        // Every field is drawn where it is laid out, and so is what the `+`
        // opened: the frame's own pixels say so, on both grounds.
        for theme in [ui::Theme::dark(), ui::Theme::light()] {
            app.harness().set_theme(theme);
            app.settle();
            let pixels = app.harness().render_rgba();
            let stride = usize::try_from(width).expect("a width that fits an index") * 4;
            let pixel = |x: f64, y: f64| {
                let column = ui::num::usize_of_f64(x);
                let row = ui::num::usize_of_f64(y);
                let offset = row * stride + column * 4;
                [
                    pixels[offset],
                    pixels[offset + 1],
                    pixels[offset + 2],
                    pixels[offset + 3],
                ]
            };
            let ground = pixel(f64::from(width) - 2.0, f64::from(height) - 2.0);
            for (id, rect) in rects.iter().chain(opened.iter()) {
                let centre = pixel(
                    f64::midpoint(rect.x0, rect.x1),
                    f64::midpoint(rect.y0, rect.y1),
                );
                assert!(
                    centre != ground && centre[3] != 0,
                    "{id} is drawn at {width} on the {} ground: {centre:?}",
                    app.harness().theme().name
                );
            }
        }
    }
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
                // A row's box is a control of its own, named the same, so the
                // rows are what counts.
                let name = node["name"].as_str().unwrap_or_default();
                (name.starts_with('#') && node["role"] == "TreeItem").then(|| name.to_owned())
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

    // Todos: the tasks nobody has started — all of them, as they were made.
    choose(&mut app, "Todos");
    let shown = rows(&mut app);
    assert_eq!(shown.len(), 3, "nothing is under way: {shown:?}");

    // An agent's: the one handed to the workspace's agent.
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

/// A lane of the board adds at its foot, in its own lane: what is typed
/// there is made in that lane's status, and the board itself carries no
/// entry of its own above the lanes.
#[test]
fn a_lanes_foot_adds_the_issue_to_that_lane() {
    let mut app = open();
    press_view(&mut app, "Board");
    app.settle();
    assert!(
        !app.harness()
            .targets()
            .contains_key("tasks-board-new-title"),
        "the board carries no entry of its own above the lanes"
    );
    let add = |app: &mut TestApp<App>, status: Status, title: &str| {
        app.harness()
            .click_named(&format!("tasks-board-new-{}-title", status.label_key()))
            .unwrap_or_else(|error| panic!("the {status:?} lane is addable: {error}"));
        app.harness().type_text(title);
        app.harness().key("Enter");
        app.settle();
    };
    add(&mut app, Status::Todo, "Ship the launch");
    add(&mut app, Status::InProgress, "Archive the banner");

    let state = app.state();
    let statuses: Vec<(&str, Status)> = state
        .issues
        .values()
        .map(|issue| (issue.title.as_str(), issue.status))
        .collect();
    assert!(
        statuses.contains(&("Ship the launch", Status::Todo)),
        "what the to-do lane's entry makes starts there: {statuses:?}"
    );
    assert!(
        statuses.contains(&("Archive the banner", Status::InProgress)),
        "and the working lane's entry starts a card that is being worked on: {statuses:?}"
    );
    assert_eq!(statuses.len(), 2, "one card per entry, nothing else");
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
    let workspace = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";
    env.set("TILO_WORKSPACE_ID", workspace);
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
        notebook.action_target(workspace, tasks::NOTES, None, "create-page")
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

/// Its palette lists what a person looks for — the tracker's resources by
/// their title and its own actions — and never the nodes its controls are
/// addressed by.
#[test]
fn its_palette_lists_its_own_resources_and_actions() {
    let mut app = open();
    app.call("create", json!({ "title": "Write the launch notes" }))
        .expect("create");
    app.harness()
        .open_search(&ui::search::SearchRequest::this_app())
        .expect("the palette opens");
    let shown: Vec<String> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    assert!(
        shown.iter().any(|label| label == "Write the launch notes"),
        "the issue by its title: {shown:?}"
    );
    assert!(
        shown.iter().any(|label| label == "Create an issue"),
        "and what the tracker does: {shown:?}"
    );
    for internal in ["Focus", "Go to", "Submit", "Search the window"] {
        assert!(
            !shown.iter().any(|label| label == internal),
            "{internal} addresses a control, it is not a row: {shown:?}"
        );
    }
    if let Some(dir) = std::env::var_os("TILO_PALETTE_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        let dir = match std::env::var_os("CARGO_TARGET_DIR") {
            Some(target) if dir.is_relative() => std::path::PathBuf::from(target).join(dir),
            _ => dir,
        };
        std::fs::create_dir_all(&dir).unwrap();
        for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
            app.harness().set_theme(theme);
            app.harness()
                .render_png(&dir.join(format!("native-palette-{shade}.png")));
            // What the palette reads row by row, for a check that cannot look at the image.
            let labels: Vec<String> = app
                .harness()
                .a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| node.label().map(str::to_owned))
                .collect();
            std::fs::write(
                dir.join(format!("native-palette-{shade}.txt")),
                labels.join("\n"),
            )
            .unwrap();
        }
    }
}

/// German is one of the languages the app ships, and it reads in German: the
/// name of the app and its copy come from `locales/de-DE.ftl`, never from the
/// English source catalogue beside it.
#[test]
fn a_german_window_reads_the_tasks_catalogue() {
    let catalog = &tasks::locales::CATALOG;
    assert!(
        catalog.languages().contains(&"de-DE"),
        "the app ships German among {:?}",
        catalog.languages()
    );
    catalog.set_locale("de-DE");
    assert_eq!(catalog.t("tasks-title"), "Aufgaben");
    assert_eq!(catalog.t("tasks-new-issue"), "Neue Aufgabe");
    catalog.set_locale("en-US");
    assert_eq!(catalog.t("tasks-title"), "Tasks");
}

#[test]
fn an_agents_create_is_shown_at_new_issue() {
    let mut app = open();
    app.call("create", json!({ "title": "Write the launch notes" }))
        .expect("create");
    app.settle();
    let targets = app.harness().targets();
    let (_, button) = targets
        .iter()
        .find(|(name, _)| name.ends_with("-create"))
        .expect("the list's New issue button");
    let locus = app
        .harness()
        .semantic_locus(&ui::headless::agent_acting("tasks.tracker", "create"))
        .expect("an agent's create finds New issue");
    assert!(
        button.contains(locus.center()),
        "the agent acts at New issue: {locus:?} in {button:?}"
    );
}
