//! Founding for sessions a test opens without a peer network: the device's own workspace service,
//! opened over the session's state as a desktop without a network opens it
//! ([`crate::sync::serve_alone`]). The service is the real one — a workspace a test makes is
//! placed on it and held as a member, as a person's made workspace is — with the workspaces' keys
//! made by the test, since a test session opens no key service of its own.

// A fixture only tests compile: the loop it waits on is a test's own.
#![cfg(all(test, target_os = "linux"))]

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_workspace_service::workspaces::DeviceKeyGroups;
use tilo_workspace_service::workspaces::fixtures::TestGroupKeys;

use crate::engine::{Engine, Founder, ServiceAnswer};

/// This device's own workspace service as a test opens it: the real service over the engine's
/// state, the keys of the workspaces' groups the test makes, and the reads and writes the
/// session's roles make through it.
pub(crate) struct TestService {
    /// The workspaces the service holds, once it opened.
    workspaces: Box<dyn Fn() -> Option<tilo_workspace_service::workspaces::Workspaces>>,
    reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    writes: Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    keys: Arc<TestGroupKeys>,
}

impl TestService {
    /// The workspaces the service holds, as this device's own roles reach them.
    #[expect(clippy::expect_used, reason = "the fixture opened the service")]
    pub(crate) fn workspaces(&self) -> tilo_workspace_service::workspaces::Workspaces {
        (self.workspaces)().expect("the device's own workspace service")
    }

    /// What reads the documents of the workspaces the service holds, as the session's roles read
    /// them.
    pub(crate) fn reads(&self) -> Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments> {
        Arc::clone(&self.reads)
    }

    /// What writes those documents, as the session's roles write them.
    pub(crate) fn writes(&self) -> Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites> {
        Arc::clone(&self.writes)
    }

    /// The keys every member of `workspace` seals its records with, as the service holds them:
    /// what this device's key service hands it once the workspace's group admits it.
    #[expect(
        clippy::expect_used,
        reason = "the fixture makes every group's keys it is asked for"
    )]
    pub(crate) fn keys_of(&self, workspace: WorkspaceId) -> key_group::ContentKeys {
        use tilo_workspace_service::workspaces::GroupKeys as _;
        let workspace = workspace.to_string();
        pollster::block_on(
            self.keys
                .group_keys(&workspace, collab_types::EVERY_MEMBER, true),
        )
        .expect("the fixture answers for every group")
        .expect("the workspace's keys")
    }

    /// Hand the service and `engine` `keys` as `workspace`'s, as the key service hands a member
    /// the keys of a group that admitted it.
    pub(crate) fn hand_keys(
        &self,
        engine: &Engine,
        workspace: WorkspaceId,
        keys: key_group::ContentKeys,
    ) {
        self.keys.hand(&workspace.to_string(), keys.clone());
        engine.hand_keys(workspace, keys);
    }

    /// Found `workspace` as `name` on the service, held here as its member, and hand `engine` the
    /// keys of its group.
    #[expect(
        clippy::expect_used,
        reason = "a test whose workspace is not founded cannot go on"
    )]
    pub(crate) fn found(&self, engine: &Engine, workspace: WorkspaceId, name: &str) {
        pollster::block_on(engine.found(workspace, name)).expect("the workspace is founded");
        engine.hand_keys(workspace, self.keys_of(workspace));
    }
}

/// Open this device's own workspace service for `engine`'s state, and found what it makes there.
pub(crate) fn serve(engine: &Arc<Engine>) -> TestService {
    let service = tilo_workspace_service::service::ServiceSlot::default();
    let keys = Arc::new(TestGroupKeys::default());
    crate::sync::serve_alone(
        engine,
        (
            service.clone(),
            crate::platform::records::Changes::default(),
        ),
        Some(DeviceKeyGroups {
            keys: Arc::clone(&keys) as Arc<dyn tilo_workspace_service::workspaces::GroupKeys>,
            sink: None,
        }),
        |founder| Arc::new(OnRuntime(founder)),
    );
    let reads = Arc::new(
        tilo_workspace_service::apps::service_reads::ServiceDocuments::on(service.clone()),
    );
    let writes =
        Arc::new(tilo_workspace_service::apps::service_writes::ServiceWrites::on(service.clone()));
    TestService {
        workspaces: Box::new(move || {
            service
                .service()
                .map(|service| service.workspaces().clone())
        }),
        reads,
        writes,
        keys,
    }
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
        target: tilo_actions::remote::RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ServiceAnswer> {
        self.0.ask(workspace, target, arguments)
    }
}
