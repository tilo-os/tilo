//! Actual runtime session factory and broker policy, with synthetic held identity
//! and a recording launcher. Receipt signing uses an isolated identity store;
//! no installed app or application process launch is involved.
use super::*;
use crate::standalone::RuntimeActionProvider;
use serde_json::json;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::{
    certificate_authority::CertificateAuthority,
    collab_app_broker::AppBrokerState,
    policy_clock::{OwnerPolicyClock, PolicyTime},
    services::{LaunchableApplication, LauncherServices},
    test_support::{Workspace as TestWorkspace, WorkspaceTree},
};

pub(in crate::standalone) struct FixtureAuthority {
    pub(in crate::standalone) workspace: uuid::Uuid,
    pub(in crate::standalone) cert: DeviceCert,
    pub(in crate::standalone) time: PolicyTime,
}
impl CertificateAuthority for FixtureAuthority {
    fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
        self.time.now_ns().map_err(|error| error.to_string())?;
        Ok(Some(self.time.clone()))
    }
    fn authorize(&self, workspace: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        if workspace != self.workspace || cert != &self.cert {
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
        workspace: uuid::Uuid,
        cert: &DeviceCert,
        time: &PolicyTime,
    ) -> Result<(), String> {
        if !time.same_origin(&self.time) {
            return Err("runtime origin changed".into());
        }
        self.authorize(workspace, cert)
    }
    fn governs(&self, workspace: uuid::Uuid, _device: Option<&[u8; 32]>) -> bool {
        workspace == self.workspace
    }
}

#[derive(Default)]
struct RecordingLauncher(Mutex<Vec<(String, String)>>);
impl LauncherServices for RecordingLauncher {
    fn applications(&self) -> Vec<LaunchableApplication> {
        vec![LaunchableApplication {
            capabilities: Vec::new(),
            package_id: "dev.tilo.fixture".into(),
            display_name: "Fixture application".into(),
            unconfined: false,
            offers: Vec::new(),
            surface: None,
        }]
    }
    fn launch(
        &self,
        app: &str,
        workspace: &Workspace,
        _: Option<&str>,
        _: &tilo_workspace_service::apps::services::LaunchPlace,
    ) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push((app.into(), workspace.id().to_string()));
        Ok(())
    }
    fn attest_process(&self, _pid: u32, _app: &str, _workspace: &str) -> bool {
        false
    }
}

#[tokio::test]
async fn admitted_runtime_factory_preserves_workspace_launcher_and_refuses_retired_run() {
    if crate::standalone::tests::isolated_signing_identity("standalone::joined::admission_tests::admitted_runtime_factory_preserves_workspace_launcher_and_refuses_retired_run").is_none() {
        return;
    }
    let placed = WorkspaceId::new();
    let (joined, directory) = Joined::test_hosting(placed).await;
    let certificate = joined.identity.device_cert.clone();
    let actor = tilo_workspace_service::apps::collab_app_broker::person_id_for_root(
        certificate.user_public_key,
    );
    let mut workspace = TestWorkspace::new(placed, "Runtime APIs");
    workspace.set_owner(actor.clone());
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree {
        persist_path: Some(directory.path().join("workspace.json")),
        ..WorkspaceTree::default()
    };
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let origin = OwnerPolicyClock::new().unwrap().new_origin();
    let launcher = Arc::new(RecordingLauncher::default());
    let mut services = membership.services();
    services.native_certificates = Some(Arc::new(FixtureAuthority {
        workspace: uuid::Uuid::parse_str(&workspace_id).unwrap(),
        cert: certificate.clone(),
        time: origin.capture().unwrap(),
    }));
    services.launcher = Some(launcher.clone());
    let (broker, turn) = AppBrokerState::test_broker_with_authority_and_invoke_turn(
        membership.authority(),
        services,
    );
    // This is the exact factory used by Standalone::mcp_session_factory.
    let factory = RuntimeActionProvider {
        broker,
        workspace: workspace_id.clone(),
        joined,
        apps: None,
        session: None,
    }
    .session_factory(
        workspace_id.clone(),
        actor.clone(),
        origin.capture().unwrap(),
    );
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
    let query = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tilo_ls","arguments":{}}});
    let offered = server.handle(query.clone()).await.unwrap();
    assert_eq!(offered["result"]["isError"], false, "{offered}");
    assert!(
        offered.to_string().contains("Fixture application"),
        "{offered}"
    );
    let invoke = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_do","arguments":{"path":"fixture#open"}}});
    let worker = std::thread::spawn(turn);
    let opened = server.handle(invoke.clone()).await.unwrap();
    worker.join().unwrap();
    assert_eq!(opened["result"]["isError"], false, "{opened}");
    // The open reaches the launcher, in this workspace; an instance that never connects is
    // started again as the open goes on to show its window.
    let reached = launcher.0.lock().unwrap().clone();
    assert!(!reached.is_empty(), "the open reached no launcher");
    assert!(
        reached
            .iter()
            .all(|launch| *launch == ("dev.tilo.fixture".to_owned(), workspace_id.clone())),
        "{reached:?}"
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
        reached.len(),
        "a retained retired server cannot launch again"
    );
    let surviving_server = factory().unwrap();
    origin.invalidate();
    let refused = surviving_server.handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_ls","arguments":{}}})).await.unwrap();
    drop(surviving_server);
    assert!(
        refused.get("error").is_some() || refused["result"]["isError"] == true,
        "retained general API outlived runtime origin: {refused}"
    );
    assert!(
        factory().is_err(),
        "a retired runtime cannot admit another run"
    );
}

/// A workspace the runtime no longer holds is a confirmed ownership change: the
/// run ends for good, and the client is told that rather than to retry.
#[tokio::test]
async fn ownership_the_runtime_no_longer_holds_ends_the_run() {
    let workspace = WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let (joined, _directory) = Joined::test_hosting(workspace).await;
    let actor = tilo_workspace_service::apps::collab_app_broker::person_id_for_root(
        joined.test_certificate().user_public_key,
    );
    let run = joined
        .admit_mcp_execution(&workspace.to_string(), &actor)
        .unwrap();
    joined.owner.invalidate_identity(workspace);
    let refusal = run.check(&joined).unwrap_err();
    assert_eq!(refusal, super::Refusal::Ended);
    assert!(!run.caller().execution_is_live());
    assert_eq!(
        tilo_actions::remote::ActionCallError::from(refusal),
        tilo_actions::remote::ActionCallError::NotPermitted,
        "an ended run says so and is not retried"
    );
}
