#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
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

#[test]
fn an_issue_made_private_moves_into_its_own_record_and_still_shows_for_its_reader() {
    use ui::permissions::policy::{Subject, roles};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_WORKSPACE_ID", "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91");
    let door = std::sync::Arc::new(Door::default());
    let _served = ui::RemoteActions::enter(
        door.clone(),
        ui::tilo_actions::remote::Caller::local_person("ada"),
    );
    let mut app = open();
    app.call("create", json!({ "title": "Salary review" }))
        .expect("create");
    let issue = only(&app);
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open the issue");

    // Choosing its readers (Ada alone here) and making it private asks the
    // room to keep its record to them first; nothing moves until it does.
    app.harness()
        .click_named("tasks-issue-make-private")
        .expect("the issue offers to make it private");
    app.settle();
    app.harness()
        .click_named("tasks-issue-private-take")
        .expect("the readers chooser's button");
    app.settle();
    let asked = door.asked.lock().unwrap().pop().expect("the room is asked");
    assert_eq!(asked.target.address.action, roles::governance::GRANT);
    assert_eq!(app.state().issues.len(), 1);

    // Once the room keeps it, the issue moves whole into its own record.
    let permissions = app.permissions();
    let room = permissions.standing().policy.room.clone();
    let record = tasks::model::private::private_record(&issue).unwrap();
    app.set_permissions(permissions.with_grant(roles::readers_grant(
        &room,
        tasks::APP_ID,
        record.route(),
        [Subject::person("ada")],
        Subject::person("ada"),
    )));
    app.settle();
    let shown: Vec<String> = app
        .harness()
        .a11y_nodes()
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    assert!(
        app.state().issues.is_empty(),
        "the tracker keeps it no more: {shown:?}"
    );
    assert!(app.state().private.get(&issue).is_some());

    // Its reader still has it: its page says who reads it, search finds it,
    // and the list shows it among the others.
    app.settle();
    assert!(app.harness().targets().contains_key("tasks-issue-readers"));
    assert!(
        app.search_index_entries()
            .iter()
            .any(|entry| entry.title == "Salary review"),
        "search finds the private issue"
    );
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("a private issue opens as any other");
    app.harness().click_named("tasks-issue-back").unwrap();
    app.settle();
    let targets = app.harness().targets();
    assert!(targets.contains_key("tasks-list"));
    assert!(
        !targets.contains_key("tasks-list-empty"),
        "the list is not empty to its reader"
    );
}
