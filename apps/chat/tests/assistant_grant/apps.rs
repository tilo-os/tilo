//! App choices are the signed host catalogue’s resource actions.

use super::*;
use std::sync::OnceLock;
use tilo_actions::published::{Offer, PublishedAction};
use tilo_actions::remote::{CataloguePage, CatalogueRow};
use tilo_actions::{ActionKind, ActionParam, ExecutionLocus, OperationKind, ParamType};

const SUBMIT_APPS: &str = "chat-assistant-grant-submit-apps";
const CANCEL_APPS: &str = "chat-assistant-grant-cancel-apps";
const MAIL: &str = "chat-assistant-grant-app-so.tilo.mail";

fn app_catalogue(person: &Caller, agent: Arc<OnceLock<String>>) -> TestProvider {
    let signed = person.clone();
    TestProvider::new().with_catalogue(move |caller, query| {
        let Some(target) = query.target.as_ref() else {
            return Ok(CataloguePage::default());
        };
        if query.provider.as_deref() != Some("agents") || target.action != "request-app-grant" {
            return Ok(CataloguePage::default());
        }
        assert_eq!(caller, &signed);
        assert_eq!(target.resource, format!("agent:{}", agent.get().unwrap()));
        let offer = Offer {
            resource: &target.resource,
            resource_kind: "agent.grant",
            action: &target.action,
            label: "Choose apps",
            description: None,
            kind: ActionKind::Outside,
            operation: OperationKind::Effect,
            confirm: false,
            asks: None,
            keywords: Vec::new(),
            icon: None,
        };
        let mut published = PublishedAction::offered(&offer, "Assistant");
        published.params.push(
            ActionParam::required("apps", ParamType::Array, "Apps").with_schema(json!({
                "type": "array", "items": {"oneOf": [
                    {"const": ui::apps::CHAT, "title": "Chat"},
                    {"const": ui::apps::MAIL, "title": "Workspace mail"},
                    {"const": ui::apps::CALENDAR, "title": "Shared calendar"}
                ]}
            })),
        );
        let row = CatalogueRow::for_action(
            "agents",
            "agents",
            published.descriptor(ExecutionLocus::ServerPreferred),
        );
        Ok(CataloguePage::from_rows("1".into(), vec![row], &query))
    })
}

#[test]
fn the_person_chooses_optional_apps_and_submits_one_request_with_chat() {
    let person = Caller::authenticated([9; 32], [10; 32]).in_workspace(WORKSPACE);
    let status = Arc::new(Mutex::new(("missing", false)));
    let catalogue_agent = Arc::new(OnceLock::new());
    let host = grant_host_on(
        app_catalogue(&person, catalogue_agent.clone()),
        &person,
        &status,
    );
    let _served = ui::RemoteActions::enter(host.clone(), person.clone());
    let mut app = chat_for(&person);
    let mine = personal_agent(&mut app, person.acting().person_id().unwrap());
    catalogue_agent.set(mine.clone()).unwrap();
    open_agent(&mut app, &mine);
    wait_for(&mut app, CHOOSE_APPS);
    app.harness().click_named(CHOOSE_APPS).unwrap();
    wait_for(&mut app, MAIL);
    let controls = app.harness().targets();
    assert!(controls.contains_key("chat-assistant-grant-app-so.tilo.calendar"));
    assert!(!controls.contains_key("chat-assistant-grant-app-so.tilo.tasks"));
    assert!(app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label()
            .is_some_and(|name| name.contains("Workspace mail"))
    }));
    app.harness().click_named(MAIL).unwrap();
    app.harness().click_named(CANCEL_APPS).unwrap();
    wait_for(&mut app, CHOOSE_APPS);
    assert!(!app.harness().targets().contains_key(SUBMIT_APPS));

    app.harness().click_named(CHOOSE_APPS).unwrap();
    wait_for(&mut app, MAIL);
    // Select, clear and select Mail again. The request must contain it once.
    for _ in 0..3 {
        app.harness().click_named(MAIL).unwrap();
    }
    let _agent_selection = app.call_by(
        ActionInvoker::agent("researcher"),
        "tilo.control:chat-assistant-grant-app-so.tilo.calendar",
        "set",
        json!({"value": true}),
    );
    let _agent_press = app.call_by(
        ActionInvoker::agent("researcher"),
        "tilo.control:chat-assistant-grant-submit-apps",
        "invoke",
        json!({}),
    );
    app.settle();
    assert_eq!(*status.lock().unwrap(), ("missing", false));
    assert!(host.asked().iter().all(|call| {
        call.target.provider != "agents" || call.target.address.action == "grant-status"
    }));

    app.harness().click_named(SUBMIT_APPS).unwrap();
    wait_for(&mut app, WITHDRAW);
    assert!(shows_approval_code(&mut app));
    assert!(!app.harness().targets().contains_key(CHOOSE_APPS));
    let writes: Vec<_> = host
        .asked()
        .into_iter()
        .filter(|call| {
            call.target.provider == "agents" && call.target.address.action != "grant-status"
        })
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].target.address.resource, format!("agent:{mine}"));
    assert_eq!(writes[0].target.address.action, "request-app-grant");
    let InvokeArgument::Json(args) = &writes[0].argument else {
        panic!("the host receives the explicit app list");
    };
    let mut chosen: Vec<String> = serde_json::from_value(args["apps"].clone()).unwrap();
    chosen.sort();
    assert_eq!(chosen, [ui::apps::CHAT, ui::apps::MAIL]);
}

#[test]
fn a_refused_app_catalogue_leaves_only_the_fixed_grant_choices() {
    let person = Caller::authenticated([11; 32], [12; 32]).in_workspace(WORKSPACE);
    let queried = Arc::new(OnceLock::new());
    let seen = queried.clone();
    let provider = TestProvider::new().with_catalogue(move |_, query| {
        if query.provider.as_deref() == Some("agents")
            && let Some(target) = query.target
            && target.action == "request-app-grant"
        {
            let _ = seen.set(target.resource);
            return Err(ActionCallError::NotPermitted);
        }
        Ok(CataloguePage::default())
    });
    let host = grant_host_on(provider, &person, &Arc::new(Mutex::new(("missing", false))));
    let _served = ui::RemoteActions::enter(host.clone(), person.clone());
    let mut app = chat_for(&person);
    let mine = personal_agent(&mut app, person.acting().person_id().unwrap());
    open_agent(&mut app, &mine);
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |_| queried
                .get()
                .is_some())
    );
    assert_eq!(queried.get().unwrap(), &format!("agent:{mine}"));
    wait_for(&mut app, REQUEST);
    app.settle();
    let controls = app.harness().targets();
    assert!(controls.contains_key(WORK));
    assert!(!controls.contains_key(CHOOSE_APPS));
    assert!(!controls.contains_key(SUBMIT_APPS));
    assert!(host.asked().iter().all(|call| {
        call.target.provider != "agents" || call.target.address.action == "grant-status"
    }));
}
