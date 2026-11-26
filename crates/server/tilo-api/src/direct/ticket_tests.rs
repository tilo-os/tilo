use super::{AnswerBody, Direct};
use crate::session::{Here, Reached, Workspace};
use crate::tests::Person;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{ActionCallError, Caller, Pending};
use tilo_device_identity::login::CertifiedAction;

const WORKSPACE: &str = "3ee4a797-1ec0-474b-a3d0-62895d5e15af";
const OTHER: &str = "79f02485-bc3c-4c65-b731-a79078c018fe";

/// The daemon has a login for WORKSPACE, but has no local workspace replica.
struct LoggedIn {
    root: [u8; 32],
    device: [u8; 32],
    accepted: Mutex<Vec<(CertifiedAction, Value)>>,
}

impl Here for LoggedIn {
    fn workspaces(&self) -> Vec<Workspace> {
        vec![]
    }

    fn enter(&self, _workspace: &str) -> Result<Reached, String> {
        Err("no local replica".into())
    }

    fn signed_ticket(
        &self,
        workspace: &str,
        caller: &Caller,
        action: CertifiedAction,
        arguments: Value,
    ) -> Option<Pending<Result<Value, ActionCallError>>> {
        assert_eq!(caller.user_public_key(), Some(&self.root));
        assert_eq!(caller.device_public_key(), Some(&self.device));
        if workspace != WORKSPACE {
            return Some(Pending::ready(Err(ActionCallError::NotPermitted)));
        }
        crate::lock(&self.accepted).push((action, arguments));
        let result = match action {
            CertifiedAction::Reported => json!({"recorded": true}),
            CertifiedAction::Pending => json!({"label": "Firefox"}),
            CertifiedAction::Approve => json!({"signed": true}),
        };
        Some(Pending::ready(Ok(result)))
    }
}

async fn post(direct: &Direct, workspace: &str, path: &str, arguments: &Value) -> (u16, Value) {
    let target = format!("/v1/workspaces/{workspace}/resources/{path}");
    let answer = direct
        .answer(
            "POST",
            &target,
            &[],
            &serde_json::to_vec(arguments).unwrap(),
        )
        .await;
    let AnswerBody::Whole(body) = answer.body else {
        panic!("an action must answer as JSON");
    };
    (answer.status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn certified_ticket_actions_reach_the_daemon_before_the_workspace_is_available_locally() {
    let person = Person::new(17);
    let daemon = Arc::new(LoggedIn {
        root: person.root.verifying_key().to_bytes(),
        device: person.device.verifying_key().to_bytes(),
        accepted: Mutex::default(),
    });
    let approve = json!({"ticket": "sign-in ticket", "cert": person.cert, "label": "Laptop"});
    let direct = Direct::here(daemon.clone(), person.device, person.cert, "Laptop").unwrap();
    let report = json!({"ticket": "login ticket", "step": "tools-added", "agent": "Codex"});
    let pending = json!({"ticket": "sign-in ticket"});

    for (route, action, arguments, result) in [
        (
            "dev%69ces/actions/reported",
            "reported",
            &report,
            json!({"recorded": true}),
        ),
        (
            "devices/actions/pending",
            "pending",
            &pending,
            json!({"label": "Firefox"}),
        ),
        (
            "devices/actions/approve",
            "approve",
            &approve,
            json!({"signed": true}),
        ),
    ] {
        let (status, answer) = post(&direct, WORKSPACE, route, arguments).await;
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["status"], "ok");
        assert_eq!(answer["path"], format!("devices#{action}"));
        assert_eq!(answer["result"], result);
    }

    let (status, refusal) = post(&direct, OTHER, "devices/actions/reported", &report).await;
    assert_eq!(status, 403, "{refusal}");
    assert_eq!(refusal["error"]["code"], "not_permitted");
    for route in ["notes/actions/reported", "devices/actions/issue"] {
        let (status, refusal) = post(&direct, WORKSPACE, route, &json!({})).await;
        assert_eq!(status, 404, "{refusal}");
        assert_eq!(refusal["error"]["code"], "not_found");
    }
    assert_eq!(
        *crate::lock(&daemon.accepted),
        [
            (CertifiedAction::Reported, report),
            (CertifiedAction::Pending, pending),
            (CertifiedAction::Approve, approve),
        ]
    );
}
