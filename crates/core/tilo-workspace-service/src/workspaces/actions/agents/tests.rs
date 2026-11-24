//! An agent's call of a workspace's governance is decided by its grants there,
//! asking its person first by default; a key a person made for their own
//! programs is that person, held to what they may.

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::{ActionKind, Answer, Grantee};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, ReceiptStatus, WORKSPACE_GOVERNANCE_KIND,
    WORKSPACE_GOVERNANCE_PROVIDER,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::tests::{
    Answering, admit, answered, caller, governed, offered, request, settled,
};
use super::super::{ADD_MEMBER, ServiceWorkspaceActions};
use crate::workspaces::tests::{governor, signed_pre_admission, test_host};
use crate::workspaces::{PreAdmitRequest, Workspaces};
use tilo_server_protocol::WorkspaceId;

/// The agent `claude` a device of `person`'s own was handed its session to.
fn agent_of(person: &DeviceCert) -> Caller {
    Caller::authenticated(person.user_public_key, person.device_public_key).delegated_to("claude")
}

/// `caller` moving `member` to Commenter: what it produced, or why not.
async fn demote(
    actions: &ServiceWorkspaceActions,
    (workspace, workspace_id): (&str, WorkspaceId),
    caller: &Caller,
    member: &DeviceCert,
) -> Result<Value, ActionCallError> {
    let asked = request(
        workspace,
        ADD_MEMBER,
        json!({"person": hex::encode(member.user_public_key), "role": "commenter"}),
    );
    in_workspace(actions, workspace_id, caller, asked).await
}

/// `request` by `caller` over the workspace's own line, answered by its person where it waits
/// for them and one answers ([`Answering`]): what it produced, or why not.
async fn in_workspace(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    caller: &Caller,
    request: tilo_actions::remote::InvokeRequest,
) -> Result<Value, ActionCallError> {
    let first = settled(
        actions
            .in_workspace(workspace_id)
            .invoke(caller, request.clone()),
    )
    .await;
    let workspaces = actions.shared.workspaces.get().unwrap();
    let receipt = answered(
        workspaces,
        (Some(workspace_id), None),
        caller,
        &request,
        first,
    )
    .await?;
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
    let allowing = Answering::approving();
    let stranger =
        Caller::delegated_agent("stranger", owner.user_public_key, owner.device_public_key);
    let refused = demote(&actions, here, &stranger, &ada).await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(allowing.asked().is_empty());

    // By default the agent asks its person first: allowed, it is done.
    demote(&actions, here, &agent, &ada).await.unwrap();
    let asked = allowing.asked();
    assert_eq!(asked.len(), 1, "the person is asked once");
    assert!(
        matches!(&asked[0].by, tilo_actions::remote::asked::AskedBy::Agent(named) if named.id == "claude"),
        "{asked:?}"
    );
    // Their no admits nothing.
    let declining = Answering::declining();
    let refused = demote(&actions, here, &agent, &ada).await;
    assert!(
        matches!(&refused, Err(ActionCallError::ConfirmationRequired(_))),
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
    let verbs = offered(&actions, workspace_id, &agent).await;
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
    let (workspace, workspace_id, actions) = governed(workspaces).await;
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
    let remove = |caller: &Caller, member: &DeviceCert| {
        let asked = request(
            &workspace,
            tilo_action_policy::roles::governance::REMOVE_MEMBER,
            json!({"person": hex::encode(member.user_public_key)}),
        );
        settled(actions.in_workspace(workspace_id).invoke(caller, asked))
    };
    // Its session says it acts for him, as every key's does.
    let removed = remove(&caller(&scripts).delegated_to("scripts"), &ada).await;
    assert!(removed.is_ok(), "{removed:?}");

    // An Editor's own key removes nobody: neither may they.
    let bo = SigningKey::from_bytes(&rand::random());
    let bo_key = device_of(&bo, "bo's scripts", DeviceCapabilities::server_key_grant());
    let_in(workspaces, &workspace, &owner, &bo_key, 4).await;
    let refused = remove(&caller(&bo_key).delegated_to("scripts"), &laptop).await;
    assert!(refused.is_err(), "{refused:?}");
    server.shutdown().await;
}

/// `answer` for every outside effect of `app`, set for the agent `claude` by
/// `owner` in the workspace's settings, under Agents: the bound the
/// workspace's list of its agents keeps for it.
async fn set_under_agents(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    app: &str,
    answer: &str,
) {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let person = hex::encode(owner.user_public_key);
    let grant = agent_answers::grant(
        workspace,
        "claude",
        (
            &AnswerScope::app(app),
            tilo_action_policy::ActionKind::Outside,
        ),
        tilo_action_policy::Answer::parse(answer).expect("an answer"),
        tilo_action_policy::Grantee::person(person),
    );
    crate::workspaces::fixtures::record_grants(workspaces, workspace, &[grant]).await;
}

#[tokio::test]
async fn what_its_person_allows_under_agents_runs_unasked_within_their_account() {
    use tilo_action_policy::agent_answers::ACCOUNT_APP;
    use tilo_server_protocol::workspaces::CREATE_WORKSPACE;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let here = (workspace.as_str(), workspace_id);
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let agent = agent_of(&owner);
    let declining = Answering::declining();
    let make = || {
        let mut asked = request("", CREATE_WORKSPACE, json!({"name": "The agent's"}));
        asked.target = tilo_actions::remote::RemoteActionTarget {
            provider: ACCOUNT_APP.into(),
            address: tilo_actions::api::ActionAddress::new(
                tilo_server_protocol::workspaces::HOST_WORKSPACES_RESOURCE,
                CREATE_WORKSPACE,
            ),
        };
        asked
    };
    let in_workspace = actions.in_workspace_as(workspace_id, &owner);

    // By default it asks its person first, who says no here.
    assert!(demote(&actions, here, &agent, &ada).await.is_err());
    let making = make();
    let first = settled(in_workspace.invoke(&agent, making.clone())).await;
    let refused = answered(
        workspaces,
        (Some(workspace_id), Some(&owner)),
        &agent,
        &making,
        first,
    )
    .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(declining.asked().len(), 2);

    // Allowed under Agents for the workspace's members and for their own
    // workspaces, it moves a member and makes a workspace unasked: what the
    // person may do themselves.
    set_under_agents(
        workspaces,
        &workspace,
        &owner,
        WORKSPACE_GOVERNANCE_PROVIDER,
        "allow",
    )
    .await;
    demote(&actions, here, &agent, &ada).await.unwrap();
    set_under_agents(workspaces, &workspace, &owner, ACCOUNT_APP, "allow").await;
    let making = make();
    let first = settled(in_workspace.invoke(&agent, making.clone())).await;
    let created = answered(
        workspaces,
        (Some(workspace_id), Some(&owner)),
        &agent,
        &making,
        first,
    )
    .await;
    assert!(created.is_ok(), "{created:?}");
    assert_eq!(declining.asked().len(), 2, "allowed, nobody is asked");
    server.shutdown().await;
}

#[tokio::test]
async fn its_person_is_asked_last_once_their_own_checks_pass() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let allowing = Answering::approving();

    // Ada removes nobody herself, so her agent is refused before she is asked:
    // her yes would be refused after.
    let remove = || {
        request(
            &workspace,
            tilo_action_policy::roles::governance::REMOVE_MEMBER,
            json!({"person": hex::encode(bo.user_public_key)}),
        )
    };
    let refused = in_workspace(&actions, workspace_id, &agent_of(&ada), remove()).await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(allowing.asked().is_empty(), "{:?}", allowing.asked());

    // An administrator may remove Bo, and their agent's grants answer Ask: it
    // asks them first, and what they refuse leaves the member in place.
    workspaces
        .set_member_administrator(
            &workspace,
            &owner,
            rand::random(),
            ada.user_public_key,
            None,
        )
        .await
        .unwrap();
    let refusing = Answering::declining();
    let refused = in_workspace(&actions, workspace_id, &agent_of(&ada), remove()).await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(refusing.asked().len(), 1, "the administrator is asked");
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(
        members
            .iter()
            .any(|member| member.root() == Some(bo.user_public_key) && !member.removed),
        "what she refused leaves Bo in place: {members:?}"
    );

    // What passes her checks is hers to answer.
    let allowing = Answering::approving();
    demote(&actions, (&workspace, workspace_id), &agent_of(&ada), &bo)
        .await
        .unwrap();
    assert_eq!(allowing.asked().len(), 1);
    server.shutdown().await;
}
