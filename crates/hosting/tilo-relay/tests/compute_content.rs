//! A compute job's inputs hydrate from its requester and its outputs return
//! by digest, through a real relay, on the production worker path. An input
//! the worker already holds runs from its own copy once the requester
//! proves it holds the same bytes, and never otherwise.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until the test ends"
)]
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use launcher::capture::{CaptureCancellation, CapturedRunOutput, CapturedTermination};
use tilo_clock::unix_now_ms;
use tilo_relay::compute_transport::MAX_COMPUTE_ADVERTISEMENT_LIFETIME_MS;
use tilo_relay::offload::WorkspaceContent;
use tilo_relay::{
    ComputeClient, ComputeContentStore, ComputeEnvelope, ComputeJobBinding, ComputeJobPhase,
    ComputeJobSpec, ComputeOutputSource, ComputeResourceLimits, ComputeResultUpdate,
    ComputeSandbox, ComputeSandboxInvocation, ComputeStatusUpdate, ComputeWorker,
    ComputeWorkerAdvertisement, ContentFetchError, FileComputeContentStore, HeldContent,
    InputStaging, InputTransfer, PowerAvailability, WorkerEnvironments, WorkerHeldContent,
    content_proof, fetch_job_content, hydrate_job_inputs, prove_job_input, run_compute_assignment,
    serve_worker_stream_with,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::compute::{FixtureCatalogue, installed_package, worker_advertisement};
use crate::common::{TestUser, client_endpoint, spawn_relay};

const WORKSPACE: u64 = 99;
const APP: &str = "org.tilo.compute-transform";
const ENTRYPOINT: &str = "uppercase";
const INPUT_BYTES: usize = 1024 * 1024;

/// What the fixture package's entrypoint does, without a nested user
/// namespace: every input the worker staged, upper-cased, as an output. It
/// sees only the job directory the worker prepared.
struct UppercaseSandbox;

impl ComputeSandbox for UppercaseSandbox {
    fn gpu_available(&self) -> bool {
        false
    }

    fn launch(
        &self,
        invocation: ComputeSandboxInvocation<'_>,
    ) -> Result<CapturedRunOutput, String> {
        use std::os::unix::process::ExitStatusExt;
        let io = invocation.host_job_dir.join(".tilo-compute");
        for entry in std::fs::read_dir(io.join("inputs")).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let bytes = std::fs::read(entry.path()).map_err(|error| error.to_string())?;
            std::fs::write(
                io.join("outputs").join(entry.file_name()),
                bytes.to_ascii_uppercase(),
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(CapturedRunOutput {
            termination: CapturedTermination::Exited(std::process::ExitStatus::from_raw(0)),
            stdout: b"transformed".to_vec(),
            stderr: Vec::new(),
        })
    }
}

/// Receive deliveries, acknowledging each, until a job's terminal result.
async fn next_result(client: &mut ComputeClient) -> ComputeResultUpdate {
    loop {
        let delivery = tilo_test_env::within("a compute delivery", client.receive())
            .await
            .unwrap();
        client.acknowledge(&delivery).await.unwrap();
        if let ComputeEnvelope::Result { result } = delivery.envelope {
            return result;
        }
    }
}

/// Report job `job_id`'s terminal `phase` from the worker.
async fn finish(
    worker: &mut ComputeClient,
    binding: &ComputeJobBinding,
    job_id: [u8; 16],
    phase: &ComputeJobPhase,
) {
    worker
        .send(&ComputeEnvelope::Result {
            result: ComputeResultUpdate {
                binding: ComputeJobBinding {
                    job_id,
                    ..binding.clone()
                },
                sequence: 2,
                phase: phase.clone(),
                log_tail: Vec::new(),
            },
        })
        .await
        .unwrap();
}

/// Take the next routed job on the worker and report it running.
async fn accept_job(worker: &mut ComputeClient, binding: &ComputeJobBinding) -> ComputeJobSpec {
    let request = tilo_test_env::within("a job on the worker", worker.receive())
        .await
        .unwrap();
    worker.acknowledge(&request).await.unwrap();
    let ComputeEnvelope::Request { spec } = request.envelope else {
        panic!("worker expected a job, got {:?}", request.envelope);
    };
    worker
        .send(&ComputeEnvelope::Status {
            update: ComputeStatusUpdate {
                binding: ComputeJobBinding {
                    job_id: spec.job_id,
                    ..binding.clone()
                },
                sequence: 1,
                phase: ComputeJobPhase::Running,
                log_tail: Vec::new(),
            },
        })
        .await
        .unwrap();
    spec
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one requester-to-worker job journey that reads top to bottom"
)]
async fn compute_job_inputs_hydrate_from_the_requester_and_outputs_return_by_digest() {
    let user = TestUser::fresh("compute content owner");
    let relay_device = user.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let requester = user.enrol("requester", DeviceCapabilities::primary());
    let requester_key = requester.device_pubkey();
    let worker = user.enrol("worker", DeviceCapabilities::host());
    let worker_key = worker.device_pubkey();
    let relay = Box::pin(spawn_relay(
        &user,
        relay_device,
        &[WORKSPACE],
        &[requester_key, worker_key],
    ))
    .await;
    let relay_address = relay.relay.node_addr();
    let root = tempfile::tempdir().unwrap();

    // The worker: one signed package, the production executor with its
    // retained-output store, serving the streams the relay opens to it.
    let (package, package_digest) = installed_package(root.path(), APP, ENTRYPOINT);
    let executor = Arc::new(ComputeWorker::new(
        worker_advertisement(worker_key, WORKSPACE, APP),
        root.path().join("data"),
        root.path().join("execution"),
        FileComputeContentStore::open(root.path().join("retained")).unwrap(),
        FixtureCatalogue(package),
        UppercaseSandbox,
    ));
    let mut worker_client = ComputeClient::connect(
        client_endpoint().await,
        relay_address.clone(),
        user.user_pk,
        Some(relay_key),
        worker.cert,
        worker.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    worker_client.attach(WORKSPACE).await.unwrap();
    assert!(matches!(
        worker_client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { .. }
    ));
    let issued_at_ms = unix_now_ms();
    worker_client
        .send(&ComputeEnvelope::Advertise {
            workspace_id: WORKSPACE,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker: worker_key,
                workspace_id: WORKSPACE,
                apps: BTreeSet::from([APP.into()]),
                accelerators: BTreeSet::new(),
                assignment_capacity: 1,
                active_assignments: 0,
                environment_capacity: 0,
                active_environments: 0,
                power: PowerAvailability::External,
                energy_saver: false,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(60_000),
            },
        })
        .await
        .unwrap();
    worker_client.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        worker_client.receive().await.unwrap().envelope,
        ComputeEnvelope::Pong
    ));
    let outputs: Arc<dyn ComputeOutputSource> = executor.clone();
    let worker_connection = worker_client.connection();
    let serving = tokio::spawn(async move {
        while let Ok((mut send, mut receive)) = worker_connection.accept_bi().await {
            let outputs = Arc::clone(&outputs);
            tokio::spawn(async move {
                let _ = serve_worker_stream_with(
                    WorkerEnvironments::default(),
                    Some(outputs),
                    &mut send,
                    &mut receive,
                )
                .await;
                let _ = send.finish();
            });
        }
    });

    // The requester: its workspace content holds the input, and it serves
    // the inputs of the jobs it submits.
    let mut requester_client = ComputeClient::connect(
        client_endpoint().await,
        relay_address,
        user.user_pk,
        Some(relay_key),
        requester.cert,
        requester.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    requester_client.attach(WORKSPACE).await.unwrap();
    assert!(matches!(
        requester_client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { .. }
    ));
    let workspace_content = FileComputeContentStore::open(root.path().join("requester")).unwrap();
    let input = (0..INPUT_BYTES)
        .map(|index| b'a' + u8::try_from(index % 26).unwrap())
        .collect::<Vec<_>>();
    let input_digest = *blake3::hash(&input).as_bytes();
    workspace_content.put(input_digest, &input).unwrap();
    requester_client.serve_inputs(Arc::new(workspace_content.clone()));

    let job = |job_id: [u8; 16], inputs: Vec<[u8; 32]>| ComputeJobSpec {
        version: 1,
        job_id,
        requester: requester_key,
        worker: worker_key,
        workspace_id: WORKSPACE,
        app_id: APP.into(),
        package_digest,
        entrypoint: ENTRYPOINT.into(),
        arguments: Vec::new(),
        inputs,
        limits: ComputeResourceLimits {
            cpu_time_ms: 5_000,
            memory_bytes: 64 * 1024 * 1024,
            output_bytes: 16 * 1024 * 1024,
            gpu: false,
        },
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(60_000),
    };
    let binding = ComputeJobBinding {
        workspace_id: WORKSPACE,
        job_id: [21; 16],
        requester: requester_key,
        worker: worker_key,
    };
    // Naming the input twice still moves it once.
    let specification = job(binding.job_id, vec![input_digest, input_digest]);
    requester_client.submit(&specification).await.unwrap();
    assert_eq!(
        accept_job(&mut worker_client, &binding).await,
        specification
    );

    // Being the worker reads nothing the job does not list.
    assert_eq!(
        fetch_job_content(
            &worker_client.connection(),
            WORKSPACE,
            specification.job_id,
            *blake3::hash(b"something else in the workspace").as_bytes(),
            u64::MAX,
        )
        .await,
        Err(ContentFetchError::Refused("not_authorised".into()))
    );

    let assignment = tilo_test_env::within(
        "the job hydrates and runs",
        run_compute_assignment(
            Arc::clone(&executor),
            worker_client.connection(),
            specification.clone(),
            root.path().join("inputs"),
            CaptureCancellation::default(),
        ),
    )
    .await;
    let expected = input.to_ascii_uppercase();
    let output_digest = *blake3::hash(&expected).as_bytes();
    assert_eq!(
        assignment.phase,
        ComputeJobPhase::Completed {
            outputs: vec![output_digest]
        },
        "{}",
        String::from_utf8_lossy(&assignment.log_tail)
    );
    assert_eq!(assignment.transfer.inputs, 1);
    assert_eq!(
        assignment.transfer.bytes,
        u64::try_from(INPUT_BYTES).unwrap()
    );
    assert!(
        !root
            .path()
            .join("inputs")
            .join(hex::encode(specification.job_id))
            .exists(),
        "hydrated inputs leave with the job"
    );
    worker_client
        .send(&ComputeEnvelope::Result {
            result: ComputeResultUpdate {
                binding: binding.clone(),
                sequence: 2,
                phase: assignment.phase.clone(),
                log_tail: assignment.log_tail.clone(),
            },
        })
        .await
        .unwrap();

    let result = next_result(&mut requester_client).await;
    let ComputeJobPhase::Completed { outputs } = result.phase else {
        panic!("job did not complete: {:?}", result.phase);
    };
    assert_eq!(outputs, vec![output_digest]);
    // The requester cannot pull its own input back through the worker.
    assert_eq!(
        requester_client
            .fetch_content(WORKSPACE, specification.job_id, input_digest, u64::MAX)
            .await,
        Err(ContentFetchError::Refused("not_authorised".into()))
    );
    let started = std::time::Instant::now();
    let received = requester_client
        .retrieve_outputs(&binding, &outputs, &workspace_content)
        .await
        .unwrap();
    let retrieval = started.elapsed();
    assert_eq!(received, u64::try_from(INPUT_BYTES).unwrap());
    assert_eq!(
        workspace_content.get(output_digest).unwrap(),
        Some(expected)
    );
    eprintln!(
        "compute content transfer: {} input(s), {} bytes hydrated in {:?}; \
         {received} output bytes retrieved in {retrieval:?}",
        assignment.transfer.inputs, assignment.transfer.bytes, assignment.transfer.elapsed,
    );

    // A worker that already holds an input runs the job on its own copy
    // once the requester proves it holds the same bytes: nothing is sent.
    let held_store = FileComputeContentStore::open(root.path().join("held")).unwrap();
    let held: Arc<dyn HeldContent> = Arc::new(held_store.clone());
    let staging = InputStaging {
        root: root.path().join("inputs"),
        held: Some(Arc::clone(&held)),
    };
    let mut next_job = 30_u8;
    // 1 MiB, and the largest whole MiB whose upper-cased output still fits
    // the job's 16 MiB output bound beside the entrypoint's stdout.
    for len in [INPUT_BYTES, 15 * INPUT_BYTES] {
        let bytes = (0..len)
            .map(|index| b'a' + u8::try_from((index + len) % 26).unwrap())
            .collect::<Vec<_>>();
        let digest = *blake3::hash(&bytes).as_bytes();
        let expected = *blake3::hash(&bytes.to_ascii_uppercase()).as_bytes();
        workspace_content.put(digest, &bytes).unwrap();
        held_store.put(digest, &bytes).unwrap();
        let mut runs = Vec::new();
        for staged in [
            InputStaging::from(root.path().join("inputs")),
            staging.clone(),
        ] {
            let spec = job([next_job; 16], vec![digest]);
            next_job += 1;
            requester_client.submit(&spec).await.unwrap();
            assert_eq!(accept_job(&mut worker_client, &binding).await, spec);
            let ran = tilo_test_env::within(
                "the job stages and runs",
                run_compute_assignment(
                    Arc::clone(&executor),
                    worker_client.connection(),
                    spec.clone(),
                    staged,
                    CaptureCancellation::default(),
                ),
            )
            .await;
            assert_eq!(
                ran.phase,
                ComputeJobPhase::Completed {
                    outputs: vec![expected]
                },
                "{}",
                String::from_utf8_lossy(&ran.log_tail)
            );
            finish(&mut worker_client, &binding, spec.job_id, &ran.phase).await;
            next_result(&mut requester_client).await;
            runs.push(ran.transfer);
        }
        let len = u64::try_from(len).unwrap();
        let (fetched, from_held) = (runs[0], runs[1]);
        assert_eq!((fetched.inputs, fetched.bytes), (1, len));
        assert_eq!((fetched.held, fetched.held_bytes), (0, 0));
        assert_eq!(
            (from_held.inputs, from_held.bytes),
            (0, 0),
            "a held input moves no bytes"
        );
        assert_eq!((from_held.held, from_held.held_bytes), (1, len));
        eprintln!(
            "compute input of {len} bytes: fetched from the requester in {:?}, \
             proven and staged from the worker's copy in {:?}",
            fetched.elapsed, from_held.elapsed,
        );
    }

    // Holding bytes is no authority: a job naming bytes only the worker
    // holds is refused a proof and fails as though the worker had none.
    let secret = b"only on the worker".to_vec();
    let secret_digest = *blake3::hash(&secret).as_bytes();
    held_store.put(secret_digest, &secret).unwrap();
    let probing = job([40; 16], vec![secret_digest]);
    requester_client.submit(&probing).await.unwrap();
    assert_eq!(accept_job(&mut worker_client, &binding).await, probing);
    assert_eq!(
        prove_job_input(
            &worker_client.connection(),
            (WORKSPACE, probing.job_id),
            secret_digest,
            [7; 32],
        )
        .await,
        Err(ContentFetchError::Refused("missing".into()))
    );
    // Nor is a proof had for a digest the job does not list.
    assert_eq!(
        prove_job_input(
            &worker_client.connection(),
            (WORKSPACE, probing.job_id),
            input_digest,
            [7; 32],
        )
        .await,
        Err(ContentFetchError::Refused("not_authorised".into()))
    );
    let probed = tilo_test_env::within(
        "a job on worker-only bytes fails",
        run_compute_assignment(
            Arc::clone(&executor),
            worker_client.connection(),
            probing.clone(),
            staging.clone(),
            CaptureCancellation::default(),
        ),
    )
    .await;
    assert_eq!(
        probed.phase,
        ComputeJobPhase::Failed {
            code: "input_unavailable".into()
        }
    );
    assert_eq!((probed.transfer.held, probed.transfer.held_bytes), (0, 0));
    finish(&mut worker_client, &binding, probing.job_id, &probed.phase).await;
    next_result(&mut requester_client).await;

    // A proof answers the worker's own challenge and nothing else.
    let listed = job([41; 16], vec![input_digest]);
    requester_client.submit(&listed).await.unwrap();
    assert_eq!(accept_job(&mut worker_client, &binding).await, listed);
    let (proof, locator) = prove_job_input(
        &worker_client.connection(),
        (WORKSPACE, listed.job_id),
        input_digest,
        [9; 32],
    )
    .await
    .unwrap();
    assert_eq!(proof, content_proof(&[9; 32], &input));
    assert_eq!(locator, None, "a plain content store offers no locator");
    finish(
        &mut worker_client,
        &binding,
        listed.job_id,
        &ComputeJobPhase::Failed {
            code: "cancelled".into(),
        },
    )
    .await;
    next_result(&mut requester_client).await;

    // An input the requester's workspace does not hold fails the job with
    // a truthful code, promptly.
    let missing = *blake3::hash(b"not in this workspace").as_bytes();
    let unavailable = job([22; 16], vec![missing]);
    requester_client.submit(&unavailable).await.unwrap();
    assert_eq!(accept_job(&mut worker_client, &binding).await, unavailable);
    let failed = tilo_test_env::within(
        "a missing input fails rather than hangs",
        run_compute_assignment(
            Arc::clone(&executor),
            worker_client.connection(),
            unavailable,
            root.path().join("inputs"),
            CaptureCancellation::default(),
        ),
    )
    .await;
    assert_eq!(
        failed.phase,
        ComputeJobPhase::Failed {
            code: "input_unavailable".into()
        }
    );
    assert!(
        String::from_utf8_lossy(&failed.log_tail).contains("missing"),
        "{}",
        String::from_utf8_lossy(&failed.log_tail)
    );
    serving.abort();
}

/// How many small files the batch below stages, in jobs of at most
/// [`BATCH_JOB_INPUTS`] inputs, and how big each is: a folder of photos
/// or documents, every one small enough to live inside the replica's
/// database rather than in a file of its own.
const BATCH_FILES: usize = 200;
const BATCH_JOB_INPUTS: usize = 50;
const BATCH_FILE_BYTES: usize = 200 * 1024;
/// How many of the batch's files the home server's replica lacks.
const BATCH_MISSING: usize = 8;

/// A compute session for `device` in [`WORKSPACE`], attached.
async fn attached_session(
    address: iroh_base::EndpointAddr,
    user: &TestUser,
    relay: [u8; 32],
    device: crate::common::TestDevice,
) -> ComputeClient {
    let mut client = ComputeClient::connect(
        client_endpoint().await,
        address,
        user.user_pk,
        Some(relay),
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    client.attach(WORKSPACE).await.unwrap();
    assert!(matches!(
        client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { .. }
    ));
    client
}

/// Advertise `worker` as able to run [`APP`] in [`WORKSPACE`].
async fn advertise(client: &mut ComputeClient, worker: [u8; 32]) {
    let issued_at_ms = unix_now_ms();
    client
        .send(&ComputeEnvelope::Advertise {
            workspace_id: WORKSPACE,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker,
                workspace_id: WORKSPACE,
                apps: BTreeSet::from([APP.into()]),
                accelerators: BTreeSet::new(),
                assignment_capacity: 1,
                active_assignments: 0,
                environment_capacity: 0,
                active_environments: 0,
                power: PowerAvailability::External,
                energy_saver: false,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(MAX_COMPUTE_ADVERTISEMENT_LIFETIME_MS),
            },
        })
        .await
        .unwrap();
    client.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        client.receive().await.unwrap().envelope,
        ComputeEnvelope::Pong
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one batch journey, fetched then from a home server's replica, that reads top to bottom"
)]
async fn a_batch_of_small_inputs_a_home_server_replica_holds_moves_no_bytes() {
    let user = TestUser::fresh("small batch owner");
    let relay_device = user.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let requester = user.enrol("laptop", DeviceCapabilities::primary());
    let requester_key = requester.device_pubkey();
    let worker = user.enrol("home server", DeviceCapabilities::host());
    let worker_key = worker.device_pubkey();
    let relay = Box::pin(spawn_relay(
        &user,
        relay_device,
        &[WORKSPACE],
        &[requester_key, worker_key],
    ))
    .await;
    let address = relay.relay.node_addr();
    let root = tempfile::tempdir().unwrap();
    let (_, package_digest) = installed_package(root.path(), APP, ENTRYPOINT);
    let mut worker_client = attached_session(address.clone(), &user, relay_key, worker).await;

    // The laptop's workspace holds the files and names each by the file
    // version its own view resolved, as an offloaded action does.
    let mut requester_client = attached_session(address, &user, relay_key, requester).await;
    let files: Vec<Vec<u8>> = (0..BATCH_FILES)
        .map(|file| {
            (0..BATCH_FILE_BYTES)
                .map(|index| u8::try_from((index * 31 + file * 7) % 251).unwrap())
                .collect()
        })
        .collect();
    let laptop_store = Arc::new(fs::store::ContentStore::memory());
    let laptop =
        WorkspaceContent::new(Arc::clone(&laptop_store), tokio::runtime::Handle::current());
    let mut digests = Vec::new();
    for bytes in &files {
        let manifest = *laptop_store
            .write_content(bytes)
            .await
            .unwrap()
            .unwrap()
            .as_bytes();
        digests.push(laptop.bind_file(manifest).await.unwrap());
    }
    requester_client.serve_inputs(Arc::new(laptop));

    // The home server's replica process keeps the workspace's store open,
    // holding every file but the last few, and serves its versions beside
    // it as `fs replica` does.
    let replica_dir = root.path().join("full-replica");
    let replica = Arc::new(fs::store::ContentStore::open(&replica_dir).await.unwrap());
    for bytes in &files[..BATCH_FILES - BATCH_MISSING] {
        replica.write_content(bytes).await.unwrap().unwrap();
    }
    let listener =
        tilo_files::native::private::bind_socket(&fs::replica_content::socket_path(&replica_dir))
            .unwrap();
    listener.set_nonblocking(true).unwrap();
    let replica_server = tokio::spawn(fs::replica_content::serve(
        tokio::net::UnixListener::from_std(listener).unwrap(),
        Arc::clone(&replica),
    ));
    let worker_held = WorkerHeldContent::new(
        Arc::new(FileComputeContentStore::open(root.path().join("retained")).unwrap()),
        &BTreeMap::from([(WORKSPACE, vec![replica_dir])]),
    );

    let binding = ComputeJobBinding {
        workspace_id: WORKSPACE,
        job_id: [0; 16],
        requester: requester_key,
        worker: worker_key,
    };
    let mut next_job = 60_u8;
    let mut runs = Vec::new();
    for held in [None, Some(worker_held.for_workspace(WORKSPACE))] {
        // A fresh advertisement for each run, so a slow host never routes
        // a job past the bounded lifetime of the last one.
        advertise(&mut worker_client, worker_key).await;
        let started = std::time::Instant::now();
        let mut total = InputTransfer::default();
        for (chunk, inputs) in digests.chunks(BATCH_JOB_INPUTS).enumerate() {
            let spec = ComputeJobSpec {
                version: 1,
                job_id: [next_job; 16],
                requester: requester_key,
                worker: worker_key,
                workspace_id: WORKSPACE,
                app_id: APP.into(),
                package_digest,
                entrypoint: ENTRYPOINT.into(),
                arguments: Vec::new(),
                inputs: inputs.to_vec(),
                limits: ComputeResourceLimits {
                    cpu_time_ms: 5_000,
                    memory_bytes: 64 * 1024 * 1024,
                    output_bytes: 16 * 1024 * 1024,
                    gpu: false,
                },
                issued_at_ms: unix_now_ms(),
                expires_at_ms: unix_now_ms().saturating_add(60_000),
            };
            next_job += 1;
            requester_client.submit(&spec).await.unwrap();
            assert_eq!(accept_job(&mut worker_client, &binding).await, spec);
            let staged = FileComputeContentStore::open(
                root.path()
                    .join("staged")
                    .join(format!("{}-{chunk}", runs.len())),
            )
            .unwrap();
            let transfer = tilo_test_env::within(
                "a batch job's inputs stage",
                hydrate_job_inputs(&worker_client.connection(), &spec, &staged, held.as_ref()),
            )
            .await
            .unwrap();
            // Whichever copy they came from, the staged inputs are the
            // laptop's bytes.
            for digest in inputs {
                let bytes = staged.get(*digest).unwrap().unwrap();
                assert_eq!(blake3::hash(&bytes).as_bytes(), digest);
            }
            total.inputs += transfer.inputs;
            total.bytes += transfer.bytes;
            total.held += transfer.held;
            total.held_bytes += transfer.held_bytes;
            let cancelled = ComputeJobPhase::Failed {
                code: "cancelled".into(),
            };
            finish(&mut worker_client, &binding, spec.job_id, &cancelled).await;
            next_result(&mut requester_client).await;
        }
        total.elapsed = started.elapsed();
        runs.push(total);
    }
    let file_bytes = u64::try_from(BATCH_FILE_BYTES).unwrap();
    let all_bytes = file_bytes * u64::try_from(BATCH_FILES).unwrap();
    let missing_bytes = file_bytes * u64::try_from(BATCH_MISSING).unwrap();
    let (fetched, from_replica) = (runs[0], runs[1]);
    assert_eq!((fetched.inputs, fetched.bytes), (BATCH_FILES, all_bytes));
    assert_eq!((fetched.held, fetched.held_bytes), (0, 0));
    // Every file the replica holds moves no bytes; only those it lacks are
    // fetched from the laptop.
    assert_eq!(
        (from_replica.inputs, from_replica.bytes),
        (BATCH_MISSING, missing_bytes),
        "only the files the replica lacks are fetched"
    );
    assert_eq!(
        (from_replica.held, from_replica.held_bytes),
        (BATCH_FILES - BATCH_MISSING, all_bytes - missing_bytes)
    );
    eprintln!(
        "small-file batch ({BATCH_FILES} files of {BATCH_FILE_BYTES} bytes, \
         {BATCH_JOB_INPUTS} per job): fetched from the requester, {} bytes in {:?}; \
         on a home server's replica, {} held and {} bytes fetched in {:?} \
         (single host: requester, relay and worker share its cores)",
        fetched.bytes, fetched.elapsed, from_replica.held, from_replica.bytes, from_replica.elapsed,
    );
    replica_server.abort();
}
