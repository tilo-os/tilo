//! The signed keeper can request that their assistant run from its conversation.

use std::sync::{Arc, Mutex};

use chat::ChatShell;
use serde_json::json;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, Pending, ReceiptStatus,
    TestProvider,
};
use tilo_actions::{ActionInvoker, ActionOutput};
use ui::framework::testing::TestApp;
use ui::tilo_workspace_agent::{self as workspace_agent, WorkspaceAgents};

mod apps;

const WORKSPACE: &str = "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90";
const REQUEST: &str = "chat-assistant-grant-request";
const REFRESH: &str = "chat-assistant-grant-refresh";
const WORK: &str = "chat-assistant-grant-work";
const CHOOSE_APPS: &str = "chat-assistant-grant-choose-apps";
const MANAGE: &str = "chat-assistant-grant-manage";
const REVOKE: &str = "chat-assistant-grant-revoke";
const WITHDRAW: &str = "chat-assistant-grant-withdraw";
const CODE: &str = "chat-assistant-grant-code";
const APPROVAL_CODE: &str = "012345";

fn wait_for(app: &mut TestApp<ChatShell>, control: &str) {
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |frame| {
                frame.targets().contains_key(control)
            }),
        "the conversation shows {control}"
    );
}

fn open_agent(app: &mut TestApp<ChatShell>, agent: &str) {
    app.call_on("chat.workspace", "open-agent", json!({"agent": agent}))
        .unwrap();
    app.settle();
}

fn grants_offered(app: &mut TestApp<ChatShell>) -> bool {
    let targets = app.harness().targets();
    [
        REQUEST,
        REFRESH,
        WORK,
        CHOOSE_APPS,
        MANAGE,
        REVOKE,
        WITHDRAW,
        CODE,
    ]
    .iter()
    .any(|control| targets.contains_key(*control))
}

fn shows_approval_code(app: &mut TestApp<ChatShell>) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        [node.label(), node.value(), node.description()]
            .into_iter()
            .flatten()
            .any(|text| text.contains(APPROVAL_CODE))
    })
}

fn chat_for(person: &Caller) -> TestApp<ChatShell> {
    let mut app =
        TestApp::<ChatShell>::for_person_in(person.acting().person_id().unwrap(), WORKSPACE)
            .unwrap();
    app.harness().resize((1060, 860)).unwrap();
    app.frame();
    app
}

fn personal_agent(app: &mut TestApp<ChatShell>, keeper: &str) -> String {
    let roster = app
        .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
        .unwrap();
    roster
        .invoke_as(
            &ActionInvoker::person(keeper),
            &WorkspaceAgents::ENSURE_PERSONAL_ASSISTANT.with(()),
        )
        .unwrap();
    roster
        .get()
        .personal_assistant(keeper)
        .unwrap()
        .id
        .0
        .clone()
}

fn grant_host(person: &Caller, status: &Arc<Mutex<(&'static str, bool)>>) -> Arc<TestProvider> {
    grant_host_on(TestProvider::new(), person, status)
}

fn grant_host_on(
    provider: TestProvider,
    person: &Caller,
    status: &Arc<Mutex<(&'static str, bool)>>,
) -> Arc<TestProvider> {
    let host_status = Arc::clone(status);
    let signed = person.clone();
    let allowed_apps = Mutex::new(json!([ui::apps::CHAT]));
    Arc::new(provider.answering(move |caller, request| {
        let action = request.target.address.action.as_str();
        let value = match action {
            "grant-status" | "request-chat-grant" | "request-assistant-work-grant"
            | "request-app-grant" | "revoke" => {
                assert_eq!(caller, &signed, "the host decides as this workspace's member");
                assert_eq!(request.target.provider, "agents");
                if action == "request-app-grant" {
                    let InvokeArgument::Json(args) = &request.argument else {
                        panic!("a custom request carries its chosen apps");
                    };
                    assert_eq!(args.len(), 1);
                    *allowed_apps.lock().unwrap() = args.get("apps").unwrap().clone();
                } else {
                    let no_arguments = match &request.argument {
                        InvokeArgument::None => true,
                        InvokeArgument::Json(args) => args.is_empty(),
                        InvokeArgument::Text(_) => false,
                    };
                    assert!(no_arguments, "fixed grant actions take no arguments");
                }
                let mut current = host_status.lock().unwrap();
                if action == "grant-status" {
                    Some(json!({"state": current.0, "ready": current.1, "apps": &*allowed_apps.lock().unwrap(), "expiresAtMs": null}))
                } else if action == "revoke" {
                    *current = ("revoked", false);
                    Some(json!({}))
                } else {
                    *current = ("pending", false);
                    let agent = request.target.address.resource.strip_prefix("agent:").unwrap();
                    Some(json!({"grant": agent, "pairingCode": APPROVAL_CODE}))
                }
            }
            _ => None,
        };
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed { output: ActionOutput { value, ..Default::default() } },
        }))
    }))
}

#[test]
fn the_person_requests_and_retries_execution_access_from_their_own_assistant_dm() {
    let person = Caller::authenticated([3; 32], [4; 32]).in_workspace(WORKSPACE);
    let status = Arc::new(Mutex::new(("missing", false)));
    let host = grant_host(&person, &status);
    let _served = ui::RemoteActions::enter(host.clone(), person.clone());
    let mut app = chat_for(&person);
    let mine = personal_agent(&mut app, person.acting().person_id().unwrap());
    let theirs = personal_agent(&mut app, &"b0".repeat(32));

    app.call_on("chat.workspace/channels/c_chat", "open-channel", json!({}))
        .unwrap();
    app.settle();
    assert!(!grants_offered(&mut app), "the workspace's ordinary chat");
    open_agent(&mut app, &theirs);
    assert!(!grants_offered(&mut app), "another person's assistant");
    assert!(
        host.asked()
            .iter()
            .all(|call| call.target.provider != "agents")
    );

    open_agent(&mut app, &mine);
    wait_for(&mut app, REQUEST);
    let _agent_press = app.call_by(
        ActionInvoker::agent("researcher"),
        "tilo.control:chat-assistant-grant-request",
        "invoke",
        json!({}),
    );
    app.settle();
    assert!(
        host.asked()
            .iter()
            .all(|call| call.target.address.action != "request-chat-grant"),
        "opening the conversation and an agent invoking the control both leave the decision to the keeper"
    );
    app.harness().click_named(REQUEST).unwrap();
    wait_for(&mut app, REFRESH);
    assert!(shows_approval_code(&mut app));

    *status.lock().unwrap() = ("denied", false);
    app.harness().click_named(REFRESH).unwrap();
    wait_for(&mut app, REQUEST);
    assert!(!shows_approval_code(&mut app));
    app.harness().click_named(REQUEST).unwrap();
    wait_for(&mut app, REFRESH);

    *status.lock().unwrap() = ("approved", false);
    app.harness().click_named(REFRESH).unwrap();
    app.settle();
    wait_for(&mut app, REFRESH);
    *status.lock().unwrap() = ("approved", true);
    app.harness().click_named(REFRESH).unwrap();
    wait_for(&mut app, MANAGE);
    assert!(!app.harness().targets().contains_key(REQUEST));
    assert!(!shows_approval_code(&mut app));

    let calls = host.asked();
    let grants: Vec<_> = calls
        .iter()
        .filter(|call| call.target.provider == "agents")
        .collect();
    let expected = format!("agent:{mine}");
    assert!(
        grants
            .iter()
            .all(|call| call.target.address.resource.as_str() == expected)
    );
    assert_eq!(
        grants
            .iter()
            .filter(|call| call.target.address.action == "request-chat-grant")
            .count(),
        2,
        "each request requires the keeper's click"
    );
}

#[test]
fn only_the_requesting_chat_shows_the_approval_code_and_the_person_can_withdraw() {
    let person = Caller::authenticated([7; 32], [8; 32]).in_workspace(WORKSPACE);
    let status = Arc::new(Mutex::new(("missing", false)));
    let host = grant_host(&person, &status);
    let _served = ui::RemoteActions::enter(host.clone(), person.clone());
    let mut app = chat_for(&person);
    let mine = personal_agent(&mut app, person.acting().person_id().unwrap());
    open_agent(&mut app, &mine);
    wait_for(&mut app, REQUEST);
    app.harness().click_named(REQUEST).unwrap();
    wait_for(&mut app, CODE);
    assert!(shows_approval_code(&mut app));
    app.harness().click_named(WITHDRAW).unwrap();
    wait_for(&mut app, REQUEST);
    assert!(!app.harness().targets().contains_key(CODE));
    assert!(!shows_approval_code(&mut app));
    assert_eq!(*status.lock().unwrap(), ("revoked", false));

    let calls = host.asked();
    let revoke = calls
        .iter()
        .position(|call| call.target.address.action == "revoke")
        .unwrap();
    let agent = format!("agent:{mine}");
    assert_eq!(calls[revoke].target.address.resource, agent);
    assert!(calls[revoke + 1..].iter().any(|call| {
        call.target.address.resource == agent && call.target.address.action == "grant-status"
    }));

    app.harness().click_named(REQUEST).unwrap();
    wait_for(&mut app, CODE);
    app.call_on("chat.workspace/channels/c_chat", "open-channel", json!({}))
        .unwrap();
    app.settle();
    assert!(!grants_offered(&mut app));
    assert!(!shows_approval_code(&mut app));
    open_agent(&mut app, &mine);
    wait_for(&mut app, WITHDRAW);
    assert!(!app.harness().targets().contains_key(CODE));
    assert!(!shows_approval_code(&mut app));
}

#[test]
fn changing_from_chat_to_workspace_work_requires_revocation_and_a_new_request() {
    let person = Caller::authenticated([5; 32], [6; 32]).in_workspace(WORKSPACE);
    let host = grant_host(&person, &Arc::new(Mutex::new(("approved", true))));
    let _served = ui::RemoteActions::enter(host.clone(), person.clone());
    let mut app = chat_for(&person);
    let mine = personal_agent(&mut app, person.acting().person_id().unwrap());
    open_agent(&mut app, &mine);
    wait_for(&mut app, MANAGE);
    assert!(!app.harness().targets().contains_key(WORK));
    app.harness().click_named(MANAGE).unwrap();
    wait_for(&mut app, REVOKE);
    assert!(!app.harness().targets().contains_key(CHOOSE_APPS));
    assert!(
        host.asked()
            .iter()
            .all(|call| call.target.address.action != "revoke")
    );
    app.harness().click_named(REVOKE).unwrap();
    wait_for(&mut app, WORK);
    assert!(
        host.asked()
            .iter()
            .all(|call| call.target.address.action != "request-assistant-work-grant")
    );
    app.harness().click_named(WORK).unwrap();
    wait_for(&mut app, REFRESH);
    let edits: Vec<_> = host
        .asked()
        .into_iter()
        .filter(|call| {
            call.target.provider == "agents" && call.target.address.action != "grant-status"
        })
        .map(|call| (call.target.address.resource, call.target.address.action))
        .collect();
    let agent = format!("agent:{mine}");
    assert_eq!(
        edits,
        [
            (agent.clone(), "revoke".to_owned()),
            (agent, "request-assistant-work-grant".to_owned())
        ]
    );
}

#[test]
fn a_refused_grant_read_leaves_the_person_in_chat_without_a_request_control() {
    let person = Caller::authenticated([1; 32], [2; 32]).in_workspace(WORKSPACE);
    let host = Arc::new(
        TestProvider::new().answering(|_, _| Pending::ready(Err(ActionCallError::NotPermitted))),
    );
    let _served = ui::RemoteActions::enter(host.clone(), person.clone());
    let mut app = chat_for(&person);
    let mine = personal_agent(&mut app, person.acting().person_id().unwrap());
    open_agent(&mut app, &mine);
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |_| {
                host.asked()
                    .iter()
                    .any(|call| call.target.address.action == "grant-status")
            })
    );
    app.settle();
    assert!(!grants_offered(&mut app));
    assert!(
        host.asked()
            .iter()
            .all(|call| call.target.address.action != "request-chat-grant")
    );
}
