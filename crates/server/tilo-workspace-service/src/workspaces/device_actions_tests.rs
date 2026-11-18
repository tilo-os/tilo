//! A device of a workspace asking the host, over the devices' action service,
//! whether it would run a piece of work: what the host answers, which work it
//! keeps for whom, and who may run it here.
#![expect(
    clippy::significant_drop_tightening,
    reason = "the server, its workspaces and the devices' action services are this test's subject until its last assertion"
)]

use super::fixtures::{entry_join, entry_join_as};
use super::tests::{governor, guest_device, test_host};
use super::*;

/// A device of a workspace asks the host, over the devices' action service, whether
/// it would run a piece of work; the host answers as one of the workspace's
/// member devices, with what it knows of itself.
#[tokio::test]
async fn a_member_device_asks_the_host_whether_it_would_run_work() {
    use neighbour_wire::offer::{Decline, Route, Work};
    use tilo_peer_state::actions::{ActionService, OFFER_DEADLINE};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let (member_key, member_cert) = guest_device();
    entry_join(
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
    let action_service = ActionService::new(
        member_key.clone(),
        Arc::new(tilo_actions::remote::TestProvider::new()),
    );
    let choice = tokio::time::timeout(
        Duration::from_secs(20),
        action_service.offers(
            &endpoint,
            [server.node_addr()],
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
    // actions on this action service, so it claims none.
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
    server.shutdown().await;
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
fn action_service_invocation(
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
struct Jobs {
    ran: std::sync::Mutex<Vec<Ran>>,
    /// How many more jobs it says it can start, when it keeps count.
    free: std::sync::Mutex<Option<u32>>,
}

impl Jobs {
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

impl HostJobs for Jobs {
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
        Result<tilo_actions::ActionOutput, tilo_actions::remote::ActionCallError>,
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
) -> (iroh::Endpoint, tilo_peer_state::actions::ActionService) {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap();
    let action_service = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_actions::remote::TestProvider::new()),
    );
    (endpoint, action_service)
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
    use tilo_actions::remote::ActionCallError;
    use tilo_peer_state::actions::OFFER_DEADLINE;

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("ActionService").await.unwrap();
    let jobs = Arc::new(Jobs::default());
    let serving: Arc<dyn HostJobs> = jobs.clone();
    workspaces.run_jobs(serving);

    // Three member devices of the workspace: the first two ask for work, the
    // third never does.
    let (a_key, a_cert) = guest_device();
    let (b_key, b_cert) = guest_device();
    let (c_key, c_cert) = guest_device();
    entry_join(workspaces, &cert, &created.workspace, (&a_key, &a_cert), 41).await;
    entry_join(workspaces, &cert, &created.workspace, (&b_key, &b_cert), 42).await;
    entry_join(workspaces, &cert, &created.workspace, (&c_key, &c_cert), 43).await;
    let (a_endpoint, a_action_service) = asking_device(&a_key).await;
    let (b_endpoint, b_action_service) = asking_device(&b_key).await;
    let (c_endpoint, c_action_service) = asking_device(&c_key).await;

    let route = Route::new("so.tilo.notes", "notes.root", "insert");
    let (a_input, b_input) = ([0xa1; 32], [0xb2; 32]);
    let a_offer = tilo_actions::InvocationId::new();
    let b_offer = tilo_actions::InvocationId::new();
    let host = server.node_addr();
    let ask = |action_service: &tilo_peer_state::actions::ActionService,
               endpoint: &iroh::Endpoint,
               work: &neighbour_wire::offer::Work,
               invocation: tilo_actions::InvocationId| {
        let host = host.clone();
        let route = route.clone();
        let workspace = created.workspace.clone();
        let (action_service_endpoint, action_service) = (endpoint.clone(), action_service.clone());
        let work = work.clone();
        async move {
            tokio::time::timeout(
                Duration::from_secs(20),
                action_service.offers(
                    &action_service_endpoint,
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
    let chosen = ask(
        &a_action_service,
        &a_endpoint,
        &offered_work(a_input),
        a_offer,
    )
    .await;
    assert_eq!(
        chosen.worker,
        Some(cert.device_public_key),
        "the host takes the first device's work: {chosen}"
    );
    let chosen = ask(
        &b_action_service,
        &b_endpoint,
        &offered_work(b_input),
        b_offer,
    )
    .await;
    assert_eq!(
        chosen.worker,
        Some(cert.device_public_key),
        "the host takes the second device's work: {chosen}"
    );

    let invoke = |action_service: &tilo_peer_state::actions::ActionService,
                  endpoint: &iroh::Endpoint,
                  invocation: tilo_actions::InvocationId| {
        let host = host.clone();
        let workspace = created.workspace.clone();
        let remote = Arc::new(action_service.remote(endpoint, &host, &workspace));
        let request = action_service_invocation(&workspace, invocation);
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
    let update = invoke(&a_action_service, &a_endpoint, a_offer)
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
    let update = invoke(&b_action_service, &b_endpoint, b_offer)
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
        let refused = invoke(&c_action_service, &c_endpoint, invocation).await;
        assert_eq!(
            refused,
            Err(ActionCallError::UnknownAction),
            "a device that offered nothing runs nothing"
        );
    }
    // Nor does one device run the work another offered.
    let refused = invoke(&a_action_service, &a_endpoint, b_offer).await;
    assert_eq!(
        refused,
        Err(ActionCallError::UnknownAction),
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
    server.shutdown().await;
}

/// Who may use the host's compute is the workspace's permission for the
/// action, not the kind of member the device belongs to: an editor's
/// invocation runs, a viewer's is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_workspaces_permission_for_the_action_decides_who_runs_work_here() {
    use neighbour_wire::offer::Route;
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::ActionCallError;
    use tilo_peer_state::actions::OFFER_DEADLINE;
    use tilo_server_protocol::workspaces::Role;

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Permits").await.unwrap();
    let jobs = Arc::new(Jobs::default());
    let serving: Arc<dyn HostJobs> = jobs.clone();
    workspaces.run_jobs(serving);

    let route = Route::new("so.tilo.notes", "notes.root", "insert");
    let host = server.node_addr();
    let mut closed = Vec::new();
    for (seed, role) in [(51, Role::Editor), (52, Role::Viewer)] {
        let (device_key, device_cert) = guest_device();
        entry_join_as(
            workspaces,
            &cert,
            &created.workspace,
            (&device_key, &device_cert),
            seed,
            role,
        )
        .await;
        let (endpoint, action_service) = asking_device(&device_key).await;
        let offer = tilo_actions::InvocationId::new();
        let chosen = tokio::time::timeout(
            Duration::from_secs(20),
            action_service.offers(
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

        let remote = Arc::new(action_service.remote(&endpoint, &host, &created.workspace));
        let update = tokio::time::timeout(
            Duration::from_secs(20),
            remote.invoke_until_settled(
                action_service_invocation(&created.workspace, offer),
                Duration::from_secs(20),
            ),
        )
        .await
        .expect("the invocation answers")
        .expect("the device answered");
        match role {
            Role::Viewer => assert_eq!(
                update,
                Err(ActionCallError::NotPermitted),
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
    server.shutdown().await;
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
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Busy").await.unwrap();
    let jobs = Arc::new(Jobs::at_capacity());
    let serving: Arc<dyn HostJobs> = jobs;
    workspaces.run_jobs(serving);

    let (device_key, device_cert) = guest_device();
    entry_join(
        workspaces,
        &cert,
        &created.workspace,
        (&device_key, &device_cert),
        61,
    )
    .await;
    let (endpoint, action_service) = asking_device(&device_key).await;
    let choice = tokio::time::timeout(
        Duration::from_secs(20),
        action_service.offers(
            &endpoint,
            [server.node_addr()],
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
    server.shutdown().await;
}

/// A host its person's entry says runs no work answers every offer as taking
/// none, whatever its job runner could run, so work lands only where its
/// entry allows it; the same host set to run work answers as it always did.
#[tokio::test]
async fn a_device_whose_entry_takes_no_work_declines_every_offer() {
    use neighbour_wire::offer::{Decline, Route};
    use tilo_peer_state::actions::OFFER_DEADLINE;

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Quiet").await.unwrap();
    let jobs = Arc::new(Jobs::default());
    let serving: Arc<dyn HostJobs> = jobs;
    workspaces.run_jobs(serving);

    let (device_key, device_cert) = guest_device();
    entry_join(
        workspaces,
        &cert,
        &created.workspace,
        (&device_key, &device_cert),
        62,
    )
    .await;
    let (endpoint, action_service) = asking_device(&device_key).await;
    let ask = |endpoint: &iroh::Endpoint,
               action_service: &tilo_peer_state::actions::ActionService| {
        action_service.offers(
            endpoint,
            [server.node_addr()],
            &created.workspace,
            &Route::new("so.tilo.notes", "notes.root", "insert"),
            &offered_work([0xc4; 32]),
            tilo_actions::InvocationId::new(),
            OFFER_DEADLINE,
        )
    };
    workspaces.set_offer(Offer::none());
    let declined = tokio::time::timeout(Duration::from_secs(20), ask(&endpoint, &action_service))
        .await
        .expect("the round finishes")
        .expect("the round completes");
    assert_eq!(
        declined
            .considered
            .first()
            .map(|candidate| candidate.verdict),
        Some(Err(Decline::NotTakingWork)),
        "a host the entry says runs no work takes none of it: {declined}"
    );
    assert!(declined.is_local(), "the job stays where it was asked");

    // The same host, its entry now naming what it offers, answers the runner.
    workspaces.set_offer(Offer::of(500, true, 2));
    let taking = tokio::time::timeout(Duration::from_secs(20), ask(&endpoint, &action_service))
        .await
        .expect("the round finishes")
        .expect("the round completes");
    assert!(
        taking
            .considered
            .first()
            .is_some_and(|candidate| candidate.verdict.is_ok()),
        "a host the entry says runs work offers it: {taking}"
    );

    endpoint.close().await;
    server.shutdown().await;
}
