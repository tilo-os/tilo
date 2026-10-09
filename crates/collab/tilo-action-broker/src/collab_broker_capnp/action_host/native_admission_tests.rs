//! Public registry dispatch tests; signed SDK observation is separately tested
//! against real packages and native libraries in launcher's integration target.
use super::{APPLICATION_ADMISSION_ACTION, CapnpBrokerState, app_admission_descriptor};
use crate::collab_broker_capnp::{BrokerChannels, FanoutState, PeerPublisher};
use crate::services::{HostServices, LaunchableApplication, LauncherServices};
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, RemoteActionTarget, workspace_provider,
};

struct ObservationHost {
    supported: bool,
    calls: AtomicUsize,
    launches: AtomicUsize,
}
impl LauncherServices for ObservationHost {
    fn observes_native_admission(&self) -> bool {
        self.supported
    }
    fn applications(&self) -> Vec<LaunchableApplication> {
        vec![LaunchableApplication {
            package_id: "so.tilo.observed".into(),
            display_name: "Observed".into(),
            unconfined: false,
        }]
    }
    fn launch(&self, _: &str, _: &Workspace) -> Result<(), String> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        Err("admission must never launch an application".into())
    }
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }
    fn native_admission(
        &self,
        app: &str,
        workspace: &Workspace,
    ) -> Result<serde_json::Value, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(
            serde_json::json!({"native_host_blake3": "registry-fixture", "admission": {"package_id": app, "workspace_id": workspace.room.to_string(),
            "package_root": "registry-fixture", "launcher_blake3": "registry-fixture",
            "runtime": {}, "connected_services": []}}),
        )
    }
}

fn broker(supported: bool) -> (Arc<CapnpBrokerState>, Arc<ObservationHost>, Caller, String) {
    let host = Arc::new(ObservationHost {
        supported,
        calls: AtomicUsize::new(0),
        launches: AtomicUsize::new(0),
    });
    let mut workspace = Workspace::new(WorkspaceId(8), "Admission");
    workspace.owner = Some(hex::encode([31; 32]));
    let room = workspace.room.to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let (state, _) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.shared().authority()),
        None,
        None,
        HostServices {
            launcher: Some(host.clone()),
            ..Default::default()
        },
    );
    let caller = Caller::authenticated([31; 32], [32; 32]).in_room(room.clone());
    (state, host, caller, room)
}

fn request(room: &str) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId(uuid::Uuid::new_v4()),
        target: RemoteActionTarget {
            provider: workspace_provider(room),
            address: tilo_actions::ActionAddress::new(
                "app.so.tilo.observed",
                APPLICATION_ADMISSION_ACTION,
            ),
        },
        argument: InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
    }
}

#[test]
fn native_admission_is_public_programmatic_and_refuses_unsupported_hosts() {
    let (state, _, caller, _) = broker(true);
    let action = state
        .eager_catalogue(&caller, true)
        .0
        .into_iter()
        .filter_map(|row| row.action)
        .find(|action| action.address.action == APPLICATION_ADMISSION_ACTION)
        .unwrap();
    assert_eq!(
        action.contract.permission,
        tilo_actions::PermissionRequirement::Write
    );
    assert!(
        action
            .contract
            .tags
            .iter()
            .any(|tag| tag == tilo_actions::PROGRAMMATIC_TAG)
    );
    assert!(matches!(
        action.contract.execution,
        tilo_actions::ExecutionMode::Background { .. }
    ));
    assert!(matches!(
        action.contract.result,
        tilo_actions::ResultSpec::Json { .. }
    ));
    let (state, _, caller, _) = broker(false);
    assert!(
        !state
            .eager_catalogue(&caller, true)
            .0
            .iter()
            .filter_map(|row| row.action.as_ref())
            .any(|action| action.address.action == APPLICATION_ADMISSION_ACTION)
    );
}

#[tokio::test]
async fn public_native_admission_preserves_policy_result_and_exact_invocation_replay() {
    let (state, host, caller, room) = broker(true);
    let request = request(&room);
    state
        .action_invoke(&caller, None, None, &request)
        .await
        .unwrap()
        .unwrap();
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(5))
        .unwrap();
    let output = loop {
        if let Some(observed) = state.action_observe(&caller, &request.invocation)
            && let tilo_actions::InvocationUpdate::Succeeded { output, .. } = observed.update
        {
            break output;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the actual background host result must be observed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    let descriptor = app_admission_descriptor(&host.applications().remove(0));
    descriptor.contract.result.validate(&output).unwrap();
    assert_eq!(output.value.unwrap()["admission"]["workspace_id"], room);
    state
        .action_invoke(&caller, None, None, &request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert_eq!(host.launches.load(Ordering::SeqCst), 0);
    let stranger = Caller::authenticated([99; 32], [98; 32]).in_room(room.clone());
    let denied = self::request(&room);
    assert!(matches!(
        state.action_invoke(&stranger, None, None, &denied).await,
        Some(Err(_))
    ));
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert_eq!(host.launches.load(Ordering::SeqCst), 0);
}
