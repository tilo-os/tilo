//! Founding for sessions that open no workspace service: tests whose session takes part in no
//! network still found the workspaces they make, and hold them as a member, as a desktop holds
//! the ones its own workspace service places.
//!
//! The workspace's governor signs with this device's own key, as the device's own workspace
//! service does, and admits the device through the signed join protocol.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_user_identity::DeviceCert;

use crate::engine::{Engine, Founder};

/// Found what `engine` makes from now on through a governor sharing its device's key, when the
/// key may leave its store.
pub(crate) fn serve(engine: &Engine) {
    let identity = engine.identity();
    let Some(secret) = identity.signer.endpoint_secret() else {
        return;
    };
    engine.serve_founding(Arc::new(Fixture {
        owner: engine.owner().clone(),
        certificate: identity.device_cert.clone(),
        key: SigningKey::from_bytes(&secret),
    }));
}

/// Founds a workspace on `owner` as `certificate`'s device.
struct Fixture {
    owner: NativeWorkspaceOwner,
    certificate: DeviceCert,
    key: SigningKey,
}

impl Founder for Fixture {
    fn found(
        &self,
        workspace: WorkspaceId,
        _name: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        let founded = tilo_peer_state::owner::testing::enroll_interactive_governor_in(
            &self.owner,
            &self.key,
            &self.certificate,
            &workspace.to_string(),
        )
        .map(drop);
        Box::pin(std::future::ready(founded))
    }

    fn ask(
        &self,
        _workspace: WorkspaceId,
        _action: &str,
        _arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<
        std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>,
        >,
    > {
        None
    }
}
