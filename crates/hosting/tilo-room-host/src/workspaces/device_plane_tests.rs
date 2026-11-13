//! A device of a workspace asking the host, over the devices' own plane,
//! whether it would run a piece of work: what the host answers, which work it
//! keeps for whom, and who may run it here.
#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay, its workspaces and the devices' planes are this test's subject until its last assertion"
)]

use super::fixtures::{door_join, door_join_as};
use super::tests::{governor, guest_device, test_host};
use super::*;

/// A device of a workspace asks the host, over the devices' own plane, whether
/// it would run a piece of work; the host answers as one of the workspace's
/// member devices, with what it knows of itself.
#[tokio::test]
async fn a_member_device_asks_the_host_whether_it_would_run_work() {
    use neighbour_wire::offer::{Decline, Route, Work};
    use tilo_peer_state::actions::{ActionPlane, OFFER_DEADLINE};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let (member_key, member_cert) = guest_device();
    door_join(
        workspaces,
        &cert,
        &created.workspace,
        (&member_key, &member_cert),
        9,
    )
    .await;

    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&member_key.to_bytes()))
        .bind()
        .await
        .unwrap();
    let plane = ActionPlane::new(
        member_key.clone(),
        Arc::new(tilo_actions::remote::TestHost::new()),
    );
    let choice = tokio::time::timeout(
        Duration::from_secs(20),
        plane.offers(
            &endpoint,
            [relay.node_addr()],
            &created.workspace,
            &Route::new("so.tilo.notes", "notes.root", "insert"),
            &Work::none(),
            tilo_actions::InvocationId::new(),
            OFFER_DEADLINE,
        ),
    )
    .await
    .expect("the round finishes")
    .expect("the round completes");

    // The host answered as itself: it holds the workspace, and it runs no app
    // actions on this plane, so it claims none.
    assert_eq!(choice.considered.len(), 1, "the host was asked: {choice}");
    assert_eq!(
        choice.considered[0].device, cert.device_public_key,
        "the answer came from the host's own device: {choice}"
    );
    assert_eq!(
        choice.considered[0].verdict,
        Err(Decline::AppNotInstalled),
        "the host answered with what it knows of itself, not as away: {choice}"
    );
    assert!(choice.is_local(), "nothing runs there: {choice}");

    endpoint.close().await;
    relay.shutdown().await;
}

/// The work a device offers the host: one input, and a run here it could
/// beat.
fn offered_work(input: [u8; 32]) -> neighbour_wire::offer::Work {
    neighbour_wire::offer::Work {
        inputs: vec![(input, 1_000)],
        input_bytes: 1_000,
        local_wall_ms: Some(10_000),
        offload: None,
    }
}

/// The invocation a device makes of `route` in `workspace`.
fn plane_invocation(
    workspace: &str,
    invocation: tilo_actions::InvocationId,
) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation,
        target: tilo_actions::remote::application_action_target(
            workspace,
            "so.tilo.notes",
            "notes.root",
            "insert",
        ),
        argument: tilo_actions::remote::InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// One run of the host's jobs: who asked, the invocation it named, and the
/// inputs the run was given.
type Ran = ([u8; 32], tilo_actions::InvocationId, Vec<[u8; 32]>);

/// A host's jobs that take anything offered and answer the work they were
/// given back: which work a run was given is what its answer names.
#[derive(Default)]
struct Recorded {
    ran: std::sync::Mutex<Vec<Ran>>,
    /// How many more jobs it says it can start, when it keeps count.
    free: std::sync::Mutex<Option<u32>>,
}

impl Recorded {
    /// The same jobs, already running as much as they take on.
    fn at_capacity() -> Self {
        Self {
            free: std::sync::Mutex::new(Some(0)),
            ..Self::default()
        }
    }

    /// The runs so far, as (requester, invocation, inputs).
    fn ran(&self) -> Vec<Ran> {
        std::mem::take(&mut *self.ran.lock().unwrap())
    }
}

impl HostJobs for Recorded {
    fn runs(&self, _workspace: &str, _work: &neighbour_wire::offer::Work) -> bool {
        true
    }

    fn holds(&self, _workspace: &str, work: &neighbour_wire::offer::Work) -> Vec<[u8; 32]> {
        work.inputs.iter().map(|(digest, _)| *digest).collect()
    }

    fn free_slots(&self) -> Option<u32> {
        *self.free.lock().unwrap()
    }

    fn run(
        &self,
        _workspace: &str,
        work: &neighbour_wire::offer::Work,
        requester: [u8; 32],
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::ActionOutput, tilo_actions::remote::ActionHostError>,
    > {
        let inputs: Vec<[u8; 32]> = work.inputs.iter().map(|(digest, _)| *digest).collect();
        self.ran
            .lock()
            .unwrap()
            .push((requester, request.invocation, inputs.clone()));
        let named: Vec<String> = inputs.iter().map(hex::encode).collect();
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({ "ran": named })),
            resources: Vec::new(),
        }))
    }
}

/// A device asking the workspace's devices, on an endpoint signed with its own
/// key.
async fn asking_device(
    key: &SigningKey,
) -> (iroh::Endpoint, tilo_peer_state::actions::ActionPlane) {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap();
    let plane = tilo_peer_state::actions::ActionPlane::new(
        key.clone(),
        Arc::new(tilo_actions::remote::TestHost::new()),
    );
    (endpoint, plane)
}

/// Two devices offer the host the same route over different inputs; each
/// invocation runs the work its own device offered, and no device runs work
/// another offered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one offer-and-invoke journey of two devices, which reads top to bottom"
)]
async fn each_device_runs_the_work_it_offered_and_no_other() {
    use neighbour_wire::offer::Route;
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::ActionHostError;
    use tilo_peer_state::actions::OFFER_DEADLINE;

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Plane").await.unwrap();
    let jobs = Arc::new(Recorded::default());
    let serving: Arc<dyn HostJobs> = jobs.clone();
    workspaces.run_jobs(serving);

    // Three member devices of the workspace: the first two ask for work, the
    // third never does.
    let (a_key, a_cert) = guest_device();
    let (b_key, b_cert) = guest_device();
    let (c_key, c_cert) = guest_device();
    door_join(workspaces, &cert, &created.workspace, (&a_key, &a_cert), 41).await;
    door_join(workspaces, &cert, &created.workspace, (&b_key, &b_cert), 42).await;
    door_join(workspaces, &cert, &created.workspace, (&c_key, &c_cert), 43).await;
    let (a_endpoint, a_plane) = asking_device(&a_key).await;
    let (b_endpoint, b_plane) = asking_device(&b_key).await;
    let (c_endpoint, c_plane) = asking_device(&c_key).await;

    let route = Route::new("so.tilo.notes", "notes.root", "insert");
    let (a_input, b_input) = ([0xa1; 32], [0xb2; 32]);
    let a_offer = tilo_actions::InvocationId::new();
    let b_offer = tilo_actions::InvocationId::new();
    let host = relay.node_addr();
    let ask = |plane: &tilo_peer_state::actions::ActionPlane,
               endpoint: &iroh::Endpoint,
               work: &neighbour_wire::offer::Work,
               invocation: tilo_actions::InvocationId| {
        let host = host.clone();
        let route = route.clone();
        let workspace = created.workspace.clone();
        let (plane_endpoint, plane) = (endpoint.clone(), plane.clone());
        let work = work.clone();
        async move {
            tokio::time::timeout(
                Duration::from_secs(20),
                plane.offers(
                    &plane_endpoint,
                    [host],
                    &workspace,
                    &route,
                    &work,
                    invocation,
                    OFFER_DEADLINE,
                ),
            )
            .await
            .expect("the round finishes")
            .expect("the round completes")
        }
    };

    // Both devices offer over the same route; the host takes both.
    let chosen = ask(&a_plane, &a_endpoint, &offered_work(a_input), a_offer).await;
    assert_eq!(
        chosen.worker,
        Some(cert.device_public_key),
        "the host takes the first device's work: {chosen}"
    );
    let chosen = ask(&b_plane, &b_endpoint, &offered_work(b_input), b_offer).await;
    assert_eq!(
        chosen.worker,
        Some(cert.device_public_key),
        "the host takes the second device's work: {chosen}"
    );

    let invoke = |plane: &tilo_peer_state::actions::ActionPlane,
                  endpoint: &iroh::Endpoint,
                  invocation: tilo_actions::InvocationId| {
        let host = host.clone();
        let workspace = created.workspace.clone();
        let remote = Arc::new(plane.remote(endpoint, &host, &workspace));
        let request = plane_invocation(&workspace, invocation);
        async move {
            tokio::time::timeout(
                Duration::from_secs(20),
                remote.invoke_until_settled(request, Duration::from_secs(20)),
            )
            .await
            .expect("the invocation answers")
            .expect("the device answered")
        }
    };

    // Each device's invocation runs the work that device offered.
    let update = invoke(&a_plane, &a_endpoint, a_offer)
        .await
        .expect("the host ran the first device's work");
    let InvocationUpdate::Succeeded { output, .. } = update else {
        panic!("the first device's work ran: {update:?}");
    };
    assert_eq!(
        output.value,
        Some(serde_json::json!({ "ran": [hex::encode(a_input)] })),
        "the run read the inputs the first device offered"
    );
    let update = invoke(&b_plane, &b_endpoint, b_offer)
        .await
        .expect("the host ran the second device's work");
    let InvocationUpdate::Succeeded { output, .. } = update else {
        panic!("the second device's work ran: {update:?}");
    };
    assert_eq!(
        output.value,
        Some(serde_json::json!({ "ran": [hex::encode(b_input)] })),
        "the run read the inputs the second device offered"
    );

    // A device that never offered runs nothing, whatever invocation it names:
    // not another device's, and not one of its own.
    for invocation in [a_offer, b_offer, tilo_actions::InvocationId::new()] {
        let refused = invoke(&c_plane, &c_endpoint, invocation).await;
        assert_eq!(
            refused,
            Err(ActionHostError::UnknownAction),
            "a device that offered nothing runs nothing"
        );
    }
    // Nor does one device run the work another offered.
    let refused = invoke(&a_plane, &a_endpoint, b_offer).await;
    assert_eq!(
        refused,
        Err(ActionHostError::UnknownAction),
        "an invocation runs only its own device's offer"
    );

    assert_eq!(
        jobs.ran(),
        vec![
            (a_cert.device_public_key, a_offer, vec![a_input]),
            (b_cert.device_public_key, b_offer, vec![b_input]),
        ],
        "each run was given its own device's input, once"
    );

    a_endpoint.close().await;
    b_endpoint.close().await;
    c_endpoint.close().await;
    relay.shutdown().await;
}

/// Who may use the host's compute is the workspace's permission for the
/// action, not the kind of collaborator the device belongs to: an editor's
/// invocation runs, a viewer's is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_workspaces_permission_for_the_action_decides_who_runs_work_here() {
    use neighbour_wire::offer::Route;
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::ActionHostError;
    use tilo_peer_state::actions::OFFER_DEADLINE;
    use tilo_relay_protocol::workspaces::Role;

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Permits").await.unwrap();
    let jobs = Arc::new(Recorded::default());
    let serving: Arc<dyn HostJobs> = jobs.clone();
    workspaces.run_jobs(serving);

    let route = Route::new("so.tilo.notes", "notes.root", "insert");
    let host = relay.node_addr();
    let mut closed = Vec::new();
    for (seed, role) in [(51, Role::Editor), (52, Role::Viewer)] {
        let (device_key, device_cert) = guest_device();
        door_join_as(
            workspaces,
            &cert,
            &created.workspace,
            (&device_key, &device_cert),
            seed,
            role,
        )
        .await;
        let (endpoint, plane) = asking_device(&device_key).await;
        let offer = tilo_actions::InvocationId::new();
        let chosen = tokio::time::timeout(
            Duration::from_secs(20),
            plane.offers(
                &endpoint,
                [host.clone()],
                &created.workspace,
                &route,
                &offered_work([seed; 32]),
                offer,
                OFFER_DEADLINE,
            ),
        )
        .await
        .expect("the round finishes")
        .expect("the round completes");
        assert_eq!(chosen.worker, Some(cert.device_public_key), "{chosen}");

        let remote = Arc::new(plane.remote(&endpoint, &host, &created.workspace));
        let update = tokio::time::timeout(
            Duration::from_secs(20),
            remote.invoke_until_settled(
                plane_invocation(&created.workspace, offer),
                Duration::from_secs(20),
            ),
        )
        .await
        .expect("the invocation answers")
        .expect("the device answered");
        match role {
            Role::Viewer => assert_eq!(
                update,
                Err(ActionHostError::NotPermitted),
                "a viewer may not use another device's compute"
            ),
            _ => assert!(
                matches!(update, Ok(InvocationUpdate::Succeeded { .. })),
                "an editor's work runs: {update:?}"
            ),
        }
        closed.push(endpoint);
    }
    for endpoint in closed {
        endpoint.close().await;
    }
    relay.shutdown().await;
}

/// A host already running as much as it takes on answers an offer as taken by
/// nobody, so the device that asks keeps the job where it is instead of
/// sending it to a host that would refuse it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_at_its_capacity_answers_an_offer_as_taken_by_nobody() {
    use neighbour_wire::offer::{Decline, Route};
    use tilo_peer_state::actions::OFFER_DEADLINE;

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Busy").await.unwrap();
    let jobs = Arc::new(Recorded::at_capacity());
    let serving: Arc<dyn HostJobs> = jobs;
    workspaces.run_jobs(serving);

    let (device_key, device_cert) = guest_device();
    door_join(
        workspaces,
        &cert,
        &created.workspace,
        (&device_key, &device_cert),
        61,
    )
    .await;
    let (endpoint, plane) = asking_device(&device_key).await;
    let choice = tokio::time::timeout(
        Duration::from_secs(20),
        plane.offers(
            &endpoint,
            [relay.node_addr()],
            &created.workspace,
            &Route::new("so.tilo.notes", "notes.root", "insert"),
            &offered_work([0xc3; 32]),
            tilo_actions::InvocationId::new(),
            OFFER_DEADLINE,
        ),
    )
    .await
    .expect("the round finishes")
    .expect("the round completes");

    assert_eq!(
        choice.considered.first().map(|candidate| candidate.verdict),
        Some(Err(Decline::AtCapacity)),
        "a host at its capacity says it takes no more: {choice}"
    );
    assert!(
        choice.is_local(),
        "the job stays where it was asked: {choice}"
    );

    endpoint.close().await;
    relay.shutdown().await;
}
