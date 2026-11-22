//! Which device takes a turn of work: a device whose person's entry offers
//! compute claims an offloaded invocation's turn only when everything it needs
//! is here, and a member device asking it over the device action service gets
//! the turn run there.
#![expect(
    clippy::significant_drop_tightening,
    reason = "the host, its workspaces and the devices' action services are this test's subject until its last assertion"
)]

use std::sync::Mutex;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::ActionCallError;
use tilo_resource::Id;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::Role;

use super::fixtures::entry_join_as;
use super::servers::{self, Servers};
use super::tests::{governor, guest_device, test_host};
use super::*;
use crate::work::{HostJobs, Ineligible, Job, NotTaken, Turn, takes_turn};

/// The jobs a device runs, as a test sets them: whether it runs the transform,
/// keeps the inputs and has room, and what each run was given.
struct Jobs {
    runs: bool,
    holds: bool,
    free: Option<u32>,
    ran: Mutex<Vec<Ran>>,
}

/// One run: the device it was for, the turn, and its inputs.
type Ran = ([u8; 32], tilo_actions::InvocationId, Vec<[u8; 32]>);

impl Jobs {
    fn ready() -> Self {
        Self {
            runs: true,
            holds: true,
            free: None,
            ran: Mutex::default(),
        }
    }
}

impl HostJobs for Jobs {
    fn runs(&self, _workspace: &str, _job: &Job) -> bool {
        self.runs
    }

    fn holds(&self, _workspace: &str, _job: &Job) -> bool {
        self.holds
    }

    fn free_slots(&self) -> Option<u32> {
        self.free
    }

    fn run(
        &self,
        _workspace: &str,
        job: &Job,
        requester: [u8; 32],
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::ActionOutput, tilo_actions::remote::ActionCallError>,
    > {
        self.ran
            .lock()
            .unwrap()
            .push((requester, invocation, job.inputs.clone()));
        let named: Vec<String> = job.inputs.iter().map(hex::encode).collect();
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({ "ran": named })),
            resources: Vec::new(),
        }))
    }
}

/// Photos' thumbnails of one photograph, as a device asks for them.
fn job(input: [u8; 32]) -> Job {
    Job {
        app: "so.tilo.photos".into(),
        resource: "photos.library".into(),
        action: "make-thumbnails".into(),
        offload: tilo_actions::Offload {
            app: "so.tilo.thumbnails".into(),
            entrypoint: "thumbnail-batch".into(),
            gpu: false,
            cpu_time_ms: 60_000,
            memory_bytes: 512 * 1024 * 1024,
            output_bytes: 8 * 1024 * 1024,
        },
        inputs: vec![input],
    }
}

/// The person's entry for `device` among their servers, offering compute when
/// `compute` says so: the Servers workspace founded here, the device enrolled.
async fn offer_compute(host: &super::fixtures::TestHost, device: &DeviceCert, compute: bool) {
    let workspaces = host.workspaces();
    let admin = person_workspace_id(&device.user_public_key);
    if servers::read(workspaces).await.is_err() {
        workspaces
            .create_workspace("Servers", Some(&admin))
            .await
            .unwrap();
        servers::apply(
            workspaces,
            Servers::ENROLL_SERVER.with((
                hex::encode(device.device_public_key),
                "host".into(),
                serde_json::to_string(&host.node_addr()).unwrap(),
            )),
        )
        .await
        .unwrap();
    }
    servers::apply(
        workspaces,
        Servers::SET_APPETITE.with((
            Id::from_raw(hex::encode(device.device_public_key)),
            0,
            compute,
            0,
        )),
    )
    .await
    .unwrap();
}

/// A device that may not take a turn never claims it, whichever reason holds;
/// one that may claims it, renews its own claim, takes it again when asked
/// twice, and lets it go.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_claims_a_turn_only_when_everything_it_needs_is_here() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Photos").await.unwrap().workspace;
    let (editor_key, editor) = guest_device();
    let (viewer_key, viewer) = guest_device();
    entry_join_as(
        workspaces,
        &cert,
        &workspace,
        (&editor_key, &editor),
        61,
        Role::Editor,
    )
    .await;
    entry_join_as(
        workspaces,
        &cert,
        &workspace,
        (&viewer_key, &viewer),
        62,
        Role::Viewer,
    )
    .await;
    let turn = |requester: &DeviceCert| Turn::Invocation {
        id: tilo_actions::InvocationId::new(),
        requester: requester.device_public_key,
        job: job([7; 32]),
    };
    let unclaimed = |turn: &Turn, why: Ineligible| {
        let turn = turn.clone();
        let workspace = workspace.clone();
        async move {
            let taken = takes_turn(workspaces, &workspace, &turn).await;
            assert_eq!(taken.err(), Some(NotTaken::Ineligible(why)), "{why}");
            let Turn::Invocation { id, .. } = turn;
            assert_eq!(
                workspaces
                    .lease_holder(&workspace, &format!("invocation/{}", id.0))
                    .await
                    .unwrap(),
                None,
                "a device that may not take a turn writes no claim: {why}"
            );
        }
    };

    unclaimed(&turn(&editor), Ineligible::OffersNoCompute).await;
    offer_compute(&host, &cert, false).await;
    unclaimed(&turn(&editor), Ineligible::OffersNoCompute).await;
    offer_compute(&host, &cert, true).await;
    unclaimed(&turn(&editor), Ineligible::RunsNoJobs).await;
    workspaces.run_jobs(Arc::new(Jobs {
        runs: false,
        ..Jobs::ready()
    }));
    unclaimed(&turn(&editor), Ineligible::CannotRun).await;
    workspaces.run_jobs(Arc::new(Jobs {
        holds: false,
        ..Jobs::ready()
    }));
    unclaimed(&turn(&editor), Ineligible::InputsElsewhere).await;
    workspaces.run_jobs(Arc::new(Jobs {
        free: Some(0),
        ..Jobs::ready()
    }));
    unclaimed(&turn(&editor), Ineligible::AtCapacity).await;
    workspaces.run_jobs(Arc::new(Jobs::ready()));
    unclaimed(&turn(&viewer), Ineligible::NotPermitted).await;

    let turn = turn(&editor);
    let Turn::Invocation { id, .. } = &turn;
    let lease = format!("invocation/{}", id.0);
    let claim = takes_turn(workspaces, &workspace, &turn).await.unwrap();
    assert_eq!(
        workspaces.lease_holder(&workspace, &lease).await.unwrap(),
        Some(cert.device_public_key),
        "the claim is a lease of the workspace"
    );
    assert!(claim.renew().await, "the holder renews its claim");
    let again = takes_turn(workspaces, &workspace, &turn).await;
    assert!(
        again.is_ok(),
        "a turn this device holds is its own: {:?}",
        again.err()
    );
    claim.release().await;
    assert_eq!(
        workspaces.lease_holder(&workspace, &lease).await.unwrap(),
        None,
        "a finished turn is let go"
    );
    host.shutdown().await;
}

/// A member device asks the host to take a turn over the device action
/// service: the host claims it, answers that it runs, and the device follows it
/// to the output; asked again, it answers the same run. A device the workspace
/// does not let run the action is refused, and nothing runs for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_device_asks_and_the_host_runs_its_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Photos").await.unwrap().workspace;
    offer_compute(&host, &cert, true).await;
    let jobs = Arc::new(Jobs::ready());
    workspaces.run_jobs(Arc::clone(&jobs) as Arc<dyn HostJobs>);
    let address = host.node_addr();

    let mut endpoints = Vec::new();
    for (seed, role) in [(71, Role::Editor), (72, Role::Viewer)] {
        let (device_key, device) = guest_device();
        entry_join_as(
            workspaces,
            &cert,
            &workspace,
            (&device_key, &device),
            seed,
            role,
        )
        .await;
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&device_key.to_bytes()))
            .bind()
            .await
            .unwrap();
        let actions = tilo_peer_state::actions::ActionService::new(
            device_key.clone(),
            Arc::new(tilo_actions::remote::TestProvider::new()),
        );
        let remote = Arc::new(actions.remote(&endpoint, &address, &workspace));
        let id = tilo_actions::InvocationId::new();
        let ask = || {
            let remote = Arc::clone(&remote);
            async move {
                tokio::time::timeout(
                    Duration::from_secs(20),
                    remote.invoke_until_settled(
                        job([seed; 32]).take_request(id),
                        Duration::from_secs(20),
                    ),
                )
                .await
                .expect("the host answers")
                .expect("the host answered")
            }
        };
        if role == Role::Viewer {
            let refused = ask().await;
            assert!(
                matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("does not let")),
                "a device the workspace does not let run the action is refused: {refused:?}"
            );
        } else {
            for _ in 0..2 {
                let update = ask().await.expect("the turn ran");
                let InvocationUpdate::Succeeded { output, .. } = update else {
                    panic!("the turn ran to its output: {update:?}");
                };
                assert_eq!(
                    output.value,
                    Some(serde_json::json!({ "ran": [hex::encode([seed; 32])] })),
                    "the run read the input the device named"
                );
            }
            assert_eq!(
                *jobs.ran.lock().unwrap(),
                vec![(device.device_public_key, id, vec![[seed; 32]])],
                "the turn ran once, for the device that asked"
            );
        }
        endpoints.push(endpoint);
    }
    assert_eq!(
        jobs.ran.lock().unwrap().len(),
        1,
        "nothing ran for the viewer"
    );
    for endpoint in endpoints {
        endpoint.close().await;
    }
    host.shutdown().await;
}

/// The workspace's leases as an app reads them (`workspace.leases`): the device whose claim
/// stands, by its key in hex, until the claim lapses or is let go, after which the lease names
/// no device and is left out.
#[cfg(feature = "apps")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_reads_a_leases_live_holder_until_its_claim_lapses() {
    use tilo_resource_store::wire::{Command, Reply};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    // The service answers an app's reads as this device, as a desktop's does.
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Desk").await.unwrap().workspace;
    let leases = || async {
        let reply = workspaces
            .own_resource_command(&workspace, "so.tilo.lang.desk", Command::Leases)
            .await
            .expect("the host answers the workspace's leases");
        let Reply::Content(Some(state)) = reply else {
            panic!("the leases are answered as the workspace's state: {reply:?}");
        };
        state
    };
    assert_eq!(
        leases().await,
        serde_json::json!({ "leases": {} }),
        "a workspace nobody claimed anything in holds no lease"
    );

    let lease = "ask/1";
    let ttl_ms = 30_000;
    workspaces
        .write_lease(&workspace, lease, ttl_ms)
        .await
        .unwrap();
    let holder = hex::encode(cert.device_public_key);
    assert_eq!(
        leases().await,
        serde_json::json!({ "leases": { lease: { "device": holder } } }),
        "the claim that stands names its device, as `invoker.device()` names one"
    );

    // Read past the claim's deadline and the clock skew devices allow, the claim has lapsed:
    // the lease names nobody, though the claim is still written.
    let lapsed = tilo_clock::unix_now_ns()
        .saturating_add(ttl_ms.saturating_mul(1_000_000))
        .saturating_add(tilo_peer_state::CLOCK_SKEW_NS)
        .saturating_add(1_000_000_000);
    let state = workspaces.workspace(&workspace).await.unwrap();
    let read_then =
        super::resources::leases_reading(&state.lock().await.peer(), super::system_time(lapsed));
    assert_eq!(
        read_then,
        serde_json::json!({ "leases": {} }),
        "a lapsed claim names no device"
    );

    workspaces.write_lease(&workspace, lease, 0).await.unwrap();
    assert_eq!(
        leases().await,
        serde_json::json!({ "leases": {} }),
        "a claim let go names no device either"
    );
    host.shutdown().await;
}
