//! A compute job's inputs hydrate from its requester and its outputs return
//! by digest, through a real relay, on the production worker path.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until the test ends"
)]
use std::collections::BTreeSet;
use std::sync::Arc;

use launcher::capture::{CaptureCancellation, CapturedRunOutput, CapturedTermination};
use package_store::InstalledPackage;
use tilo_clock::unix_now_ms;
use tilo_relay::{
    ComputeClient, ComputeContentStore, ComputeEnvelope, ComputeJobBinding, ComputeJobPhase,
    ComputeJobSpec, ComputeOutputSource, ComputePackageCatalogue, ComputeResourceLimits,
    ComputeResultUpdate, ComputeSandbox, ComputeSandboxInvocation, ComputeStatusUpdate,
    ComputeWorker, ComputeWorkerAdvertisement, ContentFetchError, DeviceRole,
    FileComputeContentStore, PowerAvailability, ServerAdvertisement, ServiceCapabilities,
    WorkerEnvironments, fetch_job_content, run_compute_assignment, serve_worker_stream_with,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

const WORKSPACE: u64 = 99;
const APP: &str = "org.tilo.compute-transform";
const ENTRYPOINT: &str = "uppercase";
const INPUT_BYTES: usize = 1024 * 1024;

/// The catalogue of a worker with exactly one installed package.
struct FixtureCatalogue(InstalledPackage);

impl ComputePackageCatalogue for FixtureCatalogue {
    fn resolve(&self, app_id: &str) -> Result<Option<InstalledPackage>, String> {
        Ok((self.0.id == app_id).then(|| self.0.clone()))
    }
}

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

/// A signed package installed the way the package store lays it out, and
/// the digest of its signed root.
fn installed_package(root: &std::path::Path) -> (InstalledPackage, [u8; 32]) {
    use ed25519_dalek::Signer;
    use tilo_app_package::blobs::{BlobHash, Collection};
    use tilo_app_package::signed::SignedRoot;

    let manifest = format!(
        "[package]\nid = {APP:?}\nversion = \"1.0.0\"\nkind = \"app\"\n\
         [publisher]\nidentity = \"ed25519:{}\"\n\
         [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         [sandbox.net]\nmode = \"off\"\n\
         [entrypoint]\nbin = \"bin/worker\"\ntype = \"cli\"\n\
         [cli]\ncommands = [{ENTRYPOINT:?}]\n",
        "00".repeat(32),
    );
    let collection =
        Collection::from_entries([("bin/worker".to_string(), BlobHash::of(b"signed worker"))]);
    let collection_bytes = collection.encode();
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
    let mut signed = SignedRoot::unsigned(
        BlobHash::of(manifest.as_bytes()),
        BlobHash::of(&collection_bytes),
        signing_key.verifying_key().to_bytes(),
    );
    signed.signature = Some([0; 64]);
    signed.signature = Some(signing_key.sign(&signed.signing_bytes()).to_bytes());
    let signed_bytes = signed.encode();
    let package_digest = *BlobHash::of(&signed_bytes).as_bytes();
    let layout = package_store::Layout::new(root.join("apps"));
    let id = tilo_app_package::PackageId::from_bytes(package_digest);
    let payload = layout.payload(id);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/worker"), b"signed worker").unwrap();
    let record = layout.meta_dir(id);
    std::fs::create_dir_all(&record).unwrap();
    std::fs::write(record.join("signed-root"), signed_bytes).unwrap();
    std::fs::write(record.join("manifest.toml"), manifest.as_bytes()).unwrap();
    std::fs::write(record.join("collection.qcl"), &collection_bytes).unwrap();
    let package = InstalledPackage {
        id: APP.into(),
        version: "1.0.0".into(),
        package_id_hex: hex::encode(package_digest),
        payload_root: payload,
        manifest_path: layout.manifest(id),
        publisher_key_hex: hex::encode(signing_key.verifying_key().to_bytes()),
        pinned: false,
        installed_at: 0,
        last_used: 0,
        attested_by: String::new(),
        cli_commands: vec![ENTRYPOINT.into()],
        runtime: String::new(),
        source: String::new(),
        held: false,
        payload_bytes: 0,
        system: false,
    };
    (package, package_digest)
}

fn worker_advertisement(worker: [u8; 32]) -> ServerAdvertisement {
    ServerAdvertisement {
        device: worker,
        role: DeviceRole::AlwaysOnServer,
        services: ServiceCapabilities {
            compute: true,
            ..ServiceCapabilities::default()
        },
        workspaces: BTreeSet::from([WORKSPACE]),
        apps: BTreeSet::from([APP.into()]),
        accelerators: BTreeSet::new(),
        priority: 0,
        latency_ms: 0,
        power: PowerAvailability::External,
        energy_saver: false,
        active_assignments: 0,
        assignment_capacity: 1,
        online: true,
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
    let (package, package_digest) = installed_package(root.path());
    let executor = Arc::new(ComputeWorker::new(
        worker_advertisement(worker_key),
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
            output_bytes: 4 * 1024 * 1024,
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
