//! An agent's call of a workspace's governance is decided by its grants there,
//! asking its person first by default; a key a person made for their own
//! programs is that person, held to what they may.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends"
)]

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::{ActionKind, Answer, Grantee};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, ReceiptStatus, WORKSPACE_GOVERNANCE_KIND,
    WORKSPACE_GOVERNANCE_PROVIDER,
};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_server_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::tests::{admit, caller, governed, request, settled};
use super::super::{ADD_MEMBER, HostWorkspaceActions};
use crate::workspaces::tests::{governor, signed_pre_admission, test_host};
use crate::workspaces::{AsksPeople, PersonAnswer, PreAdmitRequest, Workspaces};
use tilo_server_protocol::WorkspaceId;

/// The person at their approving devices, answering every question with
/// `answer`, and what they were asked.
pub(crate) struct Person {
    answer: ConfirmationAnswer,
    asked: Mutex<Vec<PendingConfirmation>>,
}

impl Person {
    pub(crate) fn answering(answer: ConfirmationAnswer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            asked: Mutex::default(),
        })
    }

    pub(crate) fn asked(&self) -> Vec<PendingConfirmation> {
        self.asked.lock().unwrap().clone()
    }
}

impl AsksPeople for Person {
    fn ask(&self, _person: [u8; 32], question: PendingConfirmation) -> PersonAnswer {
        self.asked.lock().unwrap().push(question);
        let answer = self.answer;
        Box::pin(async move { Ok(answer) })
    }
}

/// The agent `claude` a device of `person`'s own was handed its session to.
fn agent_of(person: &DeviceCert) -> Caller {
    Caller::authenticated(person.user_public_key, person.device_public_key).delegated_to("claude")
}

/// `caller` moving `member` to Commenter: what it produced, or why not.
async fn demote(
    actions: &HostWorkspaceActions,
    (workspace, workspace_id): (&str, WorkspaceId),
    caller: &Caller,
    member: &DeviceCert,
) -> Result<Value, ActionCallError> {
    let asked = request(
        workspace,
        ADD_MEMBER,
        json!({"person": hex::encode(member.user_public_key), "role": "commenter"}),
    );
    let receipt = settled(actions.in_workspace(workspace_id).invoke(caller, asked)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("governance completes: {other:?}"),
    }
}

/// `answer`, given by `owner` to the agent `claude` for moving members.
async fn answer_moving(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    answer: Answer,
) {
    let scope = AnswerScope::action(
        WORKSPACE_GOVERNANCE_PROVIDER,
        WORKSPACE_GOVERNANCE_KIND,
        ADD_MEMBER,
    );
    let grant = agent_answers::grant(
        workspace,
        "claude",
        (&scope, ActionKind::Outside),
        answer,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    workspaces
        .put_workspace_grants(workspace, owner, rand::random(), vec![grant])
        .await
        .unwrap();
}

#[tokio::test]
async fn an_agent_governs_as_its_grants_answer_and_asks_its_person_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let here = (workspace.as_str(), workspace_id);
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let agent = agent_of(&owner);

    // An agent nobody handed a session to holds no grant here.
    let allowing = Person::answering(ConfirmationAnswer::AllowOnce);
    workspaces.ask_people(allowing.clone());
    let stranger =
        Caller::delegated_agent("stranger", owner.user_public_key, owner.device_public_key);
    let refused = demote(&actions, here, &stranger, &ada).await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(allowing.asked().is_empty());

    // By default the agent asks its person first: allowed, it is done.
    demote(&actions, here, &agent, &ada).await.unwrap();
    let asked = allowing.asked();
    assert_eq!(asked.len(), 1, "the person is asked once");
    assert_eq!(asked[0].agent_id, "claude");
    assert_eq!(asked[0].workspace, workspace_id.to_string());
    // Their no changes nothing.
    let declining = Person::answering(ConfirmationAnswer::Deny);
    workspaces.ask_people(declining.clone());
    let refused = demote(&actions, here, &agent, &ada).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("did not allow")),
        "{refused:?}"
    );

    // Allowed in the workspace's settings, it goes unasked.
    answer_moving(workspaces, &workspace, &owner, Answer::Allow).await;
    demote(&actions, here, &agent, &ada).await.unwrap();
    assert_eq!(declining.asked().len(), 1, "allowed, nobody is asked");

    // Refused there, it is refused unasked and not offered.
    answer_moving(workspaces, &workspace, &owner, Answer::Deny).await;
    let refused = demote(&actions, here, &agent, &ada).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("Agents")),
        "{refused:?}"
    );
    assert_eq!(declining.asked().len(), 1, "refused, nobody is asked");
    let query = tilo_actions::remote::CatalogueQuery {
        provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
        max_results: 100,
        ..Default::default()
    };
    let offered = settled(actions.in_workspace(workspace_id).query(&agent, query))
        .await
        .unwrap();
    let verbs: Vec<_> = offered
        .rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect();
    assert!(!verbs.iter().any(|verb| verb == ADD_MEMBER), "{verbs:?}");
    assert!(
        verbs.iter().any(|verb| verb == super::super::MEMBERS),
        "it still reads who is in: {verbs:?}"
    );
    server.shutdown().await;
}

/// A device of `person`'s root with `capabilities`.
fn device_of(person: &SigningKey, label: &str, capabilities: DeviceCapabilities) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: SigningKey::from_bytes(&rand::random())
            .verifying_key()
            .to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, person).unwrap();
    cert
}

/// `device` let into `workspace` by its owner, under `seed`.
async fn let_in(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    device: &DeviceCert,
    seed: u8,
) {
    let token = [seed; 8];
    let issued = workspaces
        .pre_admit(
            owner,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: Some(device.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let reply = workspaces
        .admit_proven(
            issued.signed_invitation,
            device.clone(),
            rand::random(),
            &device.device_label,
            Some(token.as_slice()),
        )
        .await;
    assert!(
        matches!(
            reply,
            tilo_server_protocol::workspaces::JoinReply::Admitted { .. }
        ),
        "{reply:?}"
    );
}

#[tokio::test]
async fn a_persons_own_key_removes_a_member_as_they_may_and_no_further() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _, _) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    // Quinten administers the workspace; his scripts' key is him.
    let quinten = SigningKey::from_bytes(&rand::random());
    let laptop = device_of(&quinten, "laptop", DeviceCapabilities::member());
    let_in(workspaces, &workspace, &owner, &laptop, 2).await;
    workspaces
        .set_member_administrator(
            &workspace,
            &owner,
            rand::random(),
            laptop.user_public_key,
            None,
        )
        .await
        .unwrap();
    let scripts = device_of(&quinten, "scripts", DeviceCapabilities::server_key_grant());
    let_in(workspaces, &workspace, &owner, &scripts, 3).await;
    let remove = |member: &DeviceCert| WorkspacesRequest::Remove {
        workspace: workspace.clone(),
        request_id: rand::random(),
        user_public_key: member.user_public_key,
    };
    // Its session says it acts for him, as every key's does.
    let caller = caller(&scripts).delegated_to("scripts");
    workspaces
        .admit_page(&scripts, &caller, &remove(&ada))
        .await
        .unwrap();
    let removed = workspaces.page_workspaces(&scripts, remove(&ada)).await;
    assert!(
        !matches!(removed, WorkspacesReply::Refused(_)),
        "{removed:?}"
    );

    // An Editor's own key removes nobody: neither may they.
    let bo = SigningKey::from_bytes(&rand::random());
    let bo_key = device_of(&bo, "bo's scripts", DeviceCapabilities::server_key_grant());
    let_in(workspaces, &workspace, &owner, &bo_key, 4).await;
    let refused = workspaces.page_workspaces(&bo_key, remove(&laptop)).await;
    assert!(
        matches!(refused, WorkspacesReply::Refused(_)),
        "{refused:?}"
    );
    server.shutdown().await;
}
