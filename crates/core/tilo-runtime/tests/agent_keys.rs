//! An agent's key reaching the device's daemon: admitted only through a certificate chain to a
//! root of the workspace, never because the key says whom it acts for.

use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_actions::remote::{ActionCallError, WORKSPACE_GOVERNANCE_PROVIDER};
use tilo_user_identity::{AgentScope, DeviceCapabilities};

use crate::host;

/// An agent's delegate key that only a root nobody in the workspace knows certified, with the
/// reach of an agent of the person's, signs a call and asks it over the device link: it is
/// refused, and nothing it asked is answered.
#[test]
fn an_agent_whose_delegate_key_no_root_of_the_workspace_certified_is_refused() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let stranger = SigningKey::from_bytes(&[42; 32]);
        let (key, cert) = host::certified(
            &stranger,
            stranger.verifying_key().to_bytes(),
            "Ada's assistant",
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        );
        let mut request = host::call(
            WORKSPACE_GOVERNANCE_PROVIDER,
            tilo_actions::remote::workspace_governance_address(&workspace, "invitation.list"),
            &json!({}),
        );
        request.signed = Some(Box::new(
            tilo_server_protocol::invocation::sign_invocation_now(
                &workspace,
                &request,
                &cert,
                &key,
                tilo_clock::unix_now_ns(),
            )
            .unwrap(),
        ));
        let refused = host.dial(key, &workspace).await.send(request).await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "{refused:?}"
        );
    });
}
