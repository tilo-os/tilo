//! Actual runtime session factory and broker policy, with synthetic held identity
//! and a recording launcher. No installed app, user store or process launch.
use super::*;
use crate::standalone::RuntimeActionHost;
use serde_json::json;
use tilo_action_broker::{
    collab_broker_capnp::CapnpBrokerState,
    peer_actions::{CertificateAuthority, OwnerPolicyClock, PolicyTime},
    services::{LaunchableApplication, LauncherServices},
    test_support::{Workspace as TestWorkspace, WorkspaceId as TestWorkspaceId, WorkspaceTree},
};
use tilo_user_identity::DeviceCert;

pub(in crate::standalone) struct FixtureAuthority {
    pub(in crate::standalone) room: uuid::Uuid,
    pub(in crate::standalone) cert: DeviceCert,
    pub(in crate::standalone) time: PolicyTime,
}
impl CertificateAuthority for FixtureAuthority {
    fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
        self.time.now_ns().map_err(|error| error.to_string())?;
        Ok(Some(self.time.clone()))
    }
    fn authorize(&self, room: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        if room != self.room || cert != &self.cert {
            return Err("unheld fixture identity".into());
        }
        tilo_user_identity::verify_device_cert_at(
            cert,
            self.time.now_ns().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }
    fn authorize_with_policy_time(
        &self,
        room: uuid::Uuid,
        cert: &DeviceCert,
        time: &PolicyTime,
    ) -> Result<(), String> {
        if !time.same_origin(&self.time) {
            return Err("runtime origin changed".into());
        }
        self.authorize(room, cert)
    }
    fn governs(&self, room: uuid::Uuid, _device: Option<&[u8; 32]>) -> bool {
        room == self.room
    }
}

#[derive(Default)]
struct RecordingLauncher(Mutex<Vec<(String, String)>>);
impl LauncherServices for RecordingLauncher {
    fn applications(&self) -> Vec<LaunchableApplication> {
        vec![LaunchableApplication {
            package_id: "host:fixture".into(),
            display_name: "Fixture application".into(),
            unconfined: true,
            offers: Vec::new(),
        }]
    }
    fn launch(
        &self,
        app: &str,
        workspace: &Workspace,
        _: Option<&str>,
        _: &tilo_action_broker::services::LaunchPlace,
    ) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push((app.into(), workspace.room.to_string()));
        Ok(())
    }
    fn attest_process(&self, _pid: u32, _app: &str, _room: &str) -> bool {
        false
    }
}

#[tokio::test]
async fn admitted_runtime_factory_preserves_workspace_launcher_and_refuses_retired_run() {
    let (joined, _directory) = Joined::test_empty().await;
    let certificate = joined.custody.device_cert.clone();
    let actor =
        tilo_action_broker::collab_broker_capnp::principal_id_for_root(certificate.user_public_key);
    let mut workspace = TestWorkspace::new(TestWorkspaceId(42), "Runtime doors");
    workspace.set_owner(actor.clone());
    let room = workspace.room.to_string();
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let origin = OwnerPolicyClock::new().unwrap().new_origin();
    let launcher = Arc::new(RecordingLauncher::default());
    let mut services = membership.services();
    services.native_certificates = Some(Arc::new(FixtureAuthority {
        room: uuid::Uuid::parse_str(&room).unwrap(),
        cert: certificate.clone(),
        time: origin.capture().unwrap(),
    }));
    services.launcher = Some(launcher.clone());
    // This is the exact factory used by Standalone::mcp_session_factory.
    let factory = RuntimeActionHost {
        broker: CapnpBrokerState::test_broker_with_authority(membership.authority(), services),
        room: room.clone(),
        joined,
        session: None,
    }
    .session_factory(room.clone(), actor.clone(), origin.capture().unwrap());
    let server = factory().unwrap();
    let caller = server.execution_caller();
    assert_eq!(caller.user_public_key(), Some(&certificate.user_public_key));
    assert_eq!(
        caller.device_public_key(),
        Some(&certificate.device_public_key)
    );
    assert!(
        caller.is_foreign_device(),
        "standalone keeps its preexisting restriction even for a nonforeign certificate"
    );
    let query = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tilo_query","arguments":{"workspace":true,"detail":"full"}}});
    let offered = server.handle(query.clone()).await.unwrap();
    assert_eq!(offered["result"]["isError"], false, "{offered}");
    assert!(
        offered.to_string().contains("Fixture application"),
        "{offered}"
    );
    let invoke = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_invoke","arguments":{"workspace":true,"resource":"app.host:fixture","action":"open"}}});
    let opened = server.handle(invoke.clone()).await.unwrap();
    assert_eq!(opened["result"]["isError"], false, "{opened}");
    assert_eq!(
        launcher.0.lock().unwrap().as_slice(),
        &[("host:fixture".into(), room.clone())]
    );
    caller.retire_execution();
    for request in [query, invoke] {
        let refused = server.handle(request).await.unwrap();
        assert!(
            refused.get("error").is_some() || refused["result"]["isError"] == true,
            "{refused}"
        );
    }
    drop(server);
    assert_eq!(
        launcher.0.lock().unwrap().len(),
        1,
        "a retained retired server cannot launch again"
    );
    let surviving_server = factory().unwrap();
    origin.invalidate();
    let refused = surviving_server.handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_query","arguments":{"workspace":true}}})).await.unwrap();
    drop(surviving_server);
    assert!(
        refused.get("error").is_some() || refused["result"]["isError"] == true,
        "retained general door outlived runtime origin: {refused}"
    );
    assert!(
        factory().is_err(),
        "a retired runtime cannot admit another run"
    );
}
