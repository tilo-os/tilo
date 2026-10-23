#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
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

/// The session's door: records what it is asked and grants it.
#[derive(Default)]
struct Door {
    asked: std::sync::Mutex<Vec<ui::tilo_actions::remote::InvokeRequest>>,
}

impl ui::tilo_actions::remote::ActionHost for Door {
    fn query(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        _: ui::tilo_actions::remote::CatalogueQuery,
    ) -> ui::tilo_actions::remote::Pending<
        Result<ui::tilo_actions::remote::CataloguePage, ui::tilo_actions::remote::ActionHostError>,
    > {
        ui::tilo_actions::remote::Pending::ready(Err(
            ui::tilo_actions::remote::ActionHostError::NotPermitted,
        ))
    }

    fn invoke(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        request: ui::tilo_actions::remote::InvokeRequest,
    ) -> ui::tilo_actions::remote::Pending<
        Result<
            ui::tilo_actions::remote::InvocationReceipt,
            ui::tilo_actions::remote::ActionHostError,
        >,
    > {
        let invocation = request.invocation;
        self.asked.lock().unwrap().push(request);
        ui::tilo_actions::remote::Pending::ready(Ok(ui::tilo_actions::remote::InvocationReceipt {
            invocation,
            status: ui::tilo_actions::remote::ReceiptStatus::Completed {
                output: ui::tilo_actions::ActionOutput::default(),
            },
        }))
    }

    fn observe(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        _: &ui::tilo_actions::InvocationId,
    ) -> Option<ui::tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        _: &ui::tilo_actions::InvocationId,
    ) -> Result<(), ui::tilo_actions::remote::ActionHostError> {
        Ok(())
    }
}

/// A link names the record of the app that keeps what it links to, and
/// opens it there; a link made before links named a record opens the app's
/// own record of the workspace.
#[test]
fn a_link_opens_in_the_record_it_names() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let room = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";
    env.set("TILO_WORKSPACE_ID", room);
    let door = std::sync::Arc::new(Door::default());
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
        let asked = door.asked.lock().unwrap().pop().expect("one open");
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
