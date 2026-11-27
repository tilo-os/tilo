//! What the person services' tests build on: the workspace service's own fixtures, with the
//! person services registered on every host they open, and the helpers more than one test uses.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "a fixture failing is its test failing"
)]

use std::path::Path;

use ed25519_dalek::SigningKey;
use serde_json::Value;
use tilo_actions::InvocationId;
use tilo_actions::remote::ActionProvider as _;
use tilo_actions::remote::{ActionCallError, InvokeArgument, InvokeRequest, ReceiptStatus};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;
use tilo_workspace_records::accounts as workspace_accounts;
use tilo_workspace_service::workspaces::actions::ServiceWorkspaceActions;
use tilo_workspace_service::workspaces::fixtures as core;

pub(crate) use core::{
    Answering, TestGroupKeys, TestHost, admit, admitted_device_of, answered, approved, caller,
    create_own, device_cert, guest_device, host_dispatch, hosted_governed as hosted,
    hosting_device, install_app, let_in, member_service, open_home, record_grants, request,
    revoke_grant, revoke_grants, settled, shared_with, signed_from, signed_pre_admission,
    workspaces_served,
};

/// A host whose workspaces answer the person services' providers, as an embedder that runs
/// them opens one (`crate::register`).
pub(crate) async fn test_host(state_root: &Path, key: &SigningKey, cert: &DeviceCert) -> TestHost {
    core::test_host_with_registers(state_root, key, cert, vec![crate::register]).await
}

/// [`test_host`], whose device holds `keys`.
pub(crate) async fn test_host_holding(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    keys: std::sync::Arc<TestGroupKeys>,
) -> TestHost {
    core::test_host_holding_with_registers(state_root, key, cert, keys, vec![crate::register]).await
}

/// [`test_host`] on a device that takes part in its workspaces' key groups through its own key
/// service.
#[cfg(feature = "device-keys")]
pub(crate) async fn test_host_keyed(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
) -> (TestHost, tilo_workspace_service::own_device::KeyService) {
    core::test_host_keyed_with_registers(state_root, key, cert, vec![crate::register]).await
}

/// A request of one of the accounts actions, with `argument` when it has one.
pub(crate) fn asking(action: &str, argument: Value) -> InvokeRequest {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: workspace_accounts::target(action),
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// `request` of the person's own settings, asked by the device `device`
/// certifies in the workspace `workspace_id` carries.
pub(crate) async fn ask_own(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    device: &DeviceCert,
    verb: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let mut request = request("", "", argument);
    request.target = tilo_workspace_records::models::target(verb);
    let provider = host.device_page(workspace_id, device);
    let asking = caller(device);
    let invocation = request.invocation;
    let mut receipt = settled(provider.invoke(&asking, request)).await?;
    // An ask of the person's model is accepted, then followed until it ends.
    if matches!(receipt.status, ReceiptStatus::Accepted) {
        let now = tilo_clock::now();
        let until = now.checked_add(tilo_test_env::hang_bound()).unwrap_or(now);
        receipt = settled(tilo_action_calls::settle_with(
            &provider, &asking, invocation, None, until,
        ))
        .await
        .expect("the ask is the caller's to follow")
        .receipt();
    }
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a person's own settings answer: {other:?}"),
    }
}
