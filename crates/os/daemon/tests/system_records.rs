//! A workspace's record of its agents is the workspace service's own document, changed by the
//! service itself. A host decides every change it receives against the surface the change's app
//! publishes, so a change of the record after the one that founded it must name the workspace's
//! own app: one that names none has a name inferred from the kind's id, which publishes no
//! `create-agent`.
//!
//! This binary holds that test alone. The surface set it installs is the process's, as the
//! daemon's program installs it before it opens a workspace, and no other test's writes are
//! decided by it.
#![recursion_limit = "256"]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget, WORKSPACE_AGENT_CREATE,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::{OWNER, WorkspaceAgents};
use tilo_workspace_service::service::local_caller;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, hosting_device, test_host_holding,
};
use tilo_workspace_service::workspaces::{MemberDevice, Workspaces};

tilo_automerge_isolate::test_worker!();

/// Mount the language runtime the daemon links on `workspaces`, as its program does when it
/// opens: the records of the built-in apps are served to whoever the workspace's dispatch
/// admits.
fn mount(workspaces: &Workspaces) {
    workspaces
        .provide_tilolang(tilolang_workspace::host())
        .expect("the build's tilolang apps declare their surfaces");
    if let Some(tilolang) = workspaces.tilolang() {
        tilolang.records.serve(workspaces);
    }
}

/// What a pending answer comes to, on this runtime.
async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    tilo_test_env::within("the dispatch's answer", receiver)
        .await
        .unwrap()
        .expect("answered")
}

/// `operation` of `workspace`'s agents with `arguments`, asked by `device` over the workspaces'
/// dispatch, as every caller asks it.
async fn ask_agents(
    workspaces: &Workspaces,
    workspace: &str,
    device: &DeviceCert,
    operation: &str,
    arguments: Value,
) -> Result<Value, ActionCallError> {
    let Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, operation),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let caller = local_caller(device).in_workspace(workspace);
    let receipt = settled(workspaces.device_actions().invoke(&caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("an agent's change completes at once: {other:?}"),
    }
}

/// The owner makes an agent, which founds the workspace's record of them, and then another of
/// the same name, which changes the document the first made. Every host decides the change
/// against the surface the record's app publishes, and the host here sets that surface set as
/// the daemon's program does, so the second agent is made only if the change names the
/// workspace's own app.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_agent_is_made_in_the_record_the_first_founded() {
    let content = tempfile::tempdir().unwrap();
    tilo_workspace_service::surfaces::decide_receipt(&content.path().join("surfaces"));
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host_holding(
        state.path(),
        &key,
        &cert,
        Arc::new(TestGroupKeys::default()),
    )
    .await;
    let workspaces = host.workspaces().clone();
    mount(&workspaces);
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Team").await.unwrap().workspace;

    let first = ask_agents(
        &workspaces,
        &workspace,
        &cert,
        WORKSPACE_AGENT_CREATE,
        json!({ "name": "Scout" }),
    )
    .await
    .expect("the owner makes an agent");
    let second = ask_agents(
        &workspaces,
        &workspace,
        &cert,
        WORKSPACE_AGENT_CREATE,
        json!({ "name": "Scout" }),
    )
    .await
    .expect("a second agent of the name is made");
    assert_ne!(
        first["agent"], second["agent"],
        "each agent keeps an id of its own"
    );

    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let roster = workspaces
        .read_resource::<WorkspaceAgents>(&workspace, (OWNER, record.as_str()))
        .await
        .unwrap()
        .expect("the workspace lists its agents");
    assert_eq!(
        roster.agents.len(),
        2,
        "the record kept both agents: {:?}",
        roster.agents
    );

    host.shutdown().await;
}
