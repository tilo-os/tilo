//! Founding for sessions a test opens without a peer network: the device's own workspace service,
//! opened over the session's state as a desktop without a network opens it
//! ([`crate::sync::serve_alone`]). The service is the real one — a workspace a test makes is
//! placed on it and held as a member, as a person's made workspace is — with the workspaces' keys
//! made by the test, since a test session opens no key service of its own.

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_workspace_service::workspaces::DeviceKeyGroups;
use tilo_workspace_service::workspaces::fixtures::TestGroupKeys;

use crate::engine::{Engine, Founder, ServiceAnswer};

/// Open this device's own workspace service for `engine`'s state, and found what it makes there.
pub(crate) fn serve(engine: &Arc<Engine>) {
    crate::sync::serve_alone(
        engine,
        Some(DeviceKeyGroups {
            keys: Arc::new(TestGroupKeys::default()),
            sink: None,
        }),
        |founder| Arc::new(OnRuntime(founder)),
    );
}

/// A founder that founds on the app runtime, where a session founds: a test that calls
/// [`Engine::found`] from its own thread has no peer-network runtime.
struct OnRuntime(Arc<dyn Founder>);

impl Founder for OnRuntime {
    fn found(
        &self,
        workspace: WorkspaceId,
        name: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        let inner = Arc::clone(&self.0);
        Box::pin(async move {
            let founding = async move { inner.found(workspace, name).await };
            match tilo_rt::try_app() {
                Ok(runtime) => runtime
                    .block_on(founding)
                    .map_err(|error| error.to_string())?,
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn ask(
        &self,
        workspace: WorkspaceId,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ServiceAnswer> {
        self.0.ask(workspace, action, arguments)
    }
}
