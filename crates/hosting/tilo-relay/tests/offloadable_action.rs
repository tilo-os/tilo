//! An offloadable action — thumbnailing a batch of large images — invoked on
//! one device runs on another of its person's devices when the relay places
//! it there, hydrating its inputs from the invoker's workspace content and
//! returning its outputs by digest, and returns exactly what running it
//! locally returns.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until the test ends"
)]
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use launcher::capture::{CaptureCancellation, CapturedRunOutput, CapturedTermination};
use thumbnail::batch::{ENTRYPOINT, PACKAGE, thumbnail_batch};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, Offload,
    OperationKind,
};
use tilo_clock::unix_now_ms;
use tilo_relay::offload::{
    LocalTransform, OffloadError, OffloadJob, Offloader, OffloaderParts, RanOn, RelayPlacement,
    WorkspaceContent,
};
use tilo_relay::{
    ComputeClient, ComputeEnvelope, ComputeJobBinding, ComputeJobPhase, ComputeOutputSource,
    ComputeResultUpdate, ComputeSandbox, ComputeSandboxInvocation, ComputeStatusUpdate,
    ComputeWorker, ComputeWorkerAdvertisement, Envelope, FileComputeContentStore,
    PowerAvailability, RelayClient, WorkerEnvironments, run_compute_assignment,
    serve_worker_stream_with,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::compute::{FixtureCatalogue, installed_package, worker_advertisement};
use crate::common::{TestDevice, TestUser, client_endpoint, spawn_relay};

const WORKSPACE: u64 = 61;
/// Images in the batch, and their size: camera originals, roughly.
const IMAGES: usize = 12;
const WIDTH: u32 = 2400;
const HEIGHT: u32 = 1600;

/// The thumbnails package's entrypoint as the worker's sandbox runs it,
/// without a nested user namespace: the very function the package binary
/// calls, over the job directory the worker staged.
struct ThumbnailSandbox;

impl ComputeSandbox for ThumbnailSandbox {
    fn gpu_available(&self) -> bool {
        false
    }

    fn launch(
        &self,
        invocation: ComputeSandboxInvocation<'_>,
    ) -> Result<CapturedRunOutput, String> {
        use std::os::unix::process::ExitStatusExt;
        if invocation.entrypoint != ENTRYPOINT {
            return Err(format!("no catalogue command {}", invocation.entrypoint));
        }
        let io = invocation.host_job_dir.join(".tilo-compute");
        let (code, stderr) = match thumbnail::batch::run_compute_request(
            &io.join("request.json"),
            &io.join("outputs"),
        ) {
            Ok(()) => (0, Vec::new()),
            Err(error) => (i32::from(error.exit_code()), error.to_string().into_bytes()),
        };
        Ok(CapturedRunOutput {
            termination: CapturedTermination::Exited(std::process::ExitStatus::from_raw(
                code.checked_mul(256).unwrap(),
            )),
            stdout: Vec::new(),
            stderr,
        })
    }
}

/// The action a photo library offers for a selection: thumbnails of every
/// photograph in it, declared heavy enough to run elsewhere.
fn make_thumbnails() -> ActionDescriptor {
    ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new("photos.library:main", "thumbnails.make"),
        resource_kind: "photos.library".into(),
        resource_label: "Library".into(),
        action_label: "Make thumbnails".into(),
        description: Some("Thumbnails of the selected photographs.".into()),
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract: ActionContract {
            offload: Some(Offload {
                app: PACKAGE.into(),
                entrypoint: ENTRYPOINT.into(),
                gpu: false,
                cpu_time_ms: 120_000,
                memory_bytes: 1024 * 1024 * 1024,
                output_bytes: 16 * 1024 * 1024,
            }),
            ..ActionContract::view()
        },
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// A large photograph-like PNG, different for each `seed`: smooth
/// gradients under sensor-like grain.
fn photograph(seed: u32) -> Vec<u8> {
    let pixels = usize::try_from(WIDTH.wrapping_mul(HEIGHT)).unwrap();
    let mut rgba = Vec::with_capacity(pixels.wrapping_mul(4));
    let mut noise = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let channel = |value: u32, grain: u8| u8::try_from(value % 224).unwrap().wrapping_add(grain);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            noise ^= noise.wrapping_shl(13);
            noise ^= noise.wrapping_shr(17);
            noise ^= noise.wrapping_shl(5);
            let grain = u8::try_from(noise & 0x1f).unwrap();
            rgba.extend_from_slice(&[
                channel(x.wrapping_add(seed.wrapping_mul(37)), grain),
                channel(y.wrapping_add(seed.wrapping_mul(11)), grain),
                channel(x.wrapping_add(y), grain),
                255,
            ]);
        }
    }
    thumbnail::encode_png(&thumbnail::RgbaThumb {
        rgba,
        width: WIDTH,
        height: HEIGHT,
    })
    .unwrap()
}

/// `device`'s relay-v1 session, subscribed to [`WORKSPACE`].
async fn relay_session(
    address: iroh_base::EndpointAddr,
    user: &TestUser,
    device: &TestDevice,
) -> RelayClient {
    let client = RelayClient::connect(
        client_endpoint().await,
        address,
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: WORKSPACE,
        })
        .unwrap();
    let ready = tilo_test_env::within("the relay session", client.recv()).await;
    assert!(matches!(ready, Some(Envelope::Ready { .. })), "{ready:?}");
    client
}

/// `device`'s compute session with the relay.
async fn compute_session(
    address: iroh_base::EndpointAddr,
    user: &TestUser,
    relay: [u8; 32],
    device: TestDevice,
) -> ComputeClient {
    ComputeClient::connect(
        client_endpoint().await,
        address,
        user.user_pk,
        Some(relay),
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap()
}

type Executor = ComputeWorker<FileComputeContentStore, FixtureCatalogue, ThumbnailSandbox>;

/// Run every job routed to `worker`, as the production worker does: report
/// it running, hydrate and execute it, report its result.
async fn run_jobs(mut worker: ComputeClient, executor: Arc<Executor>, inputs: PathBuf) {
    while let Ok(delivery) = worker.receive().await {
        worker.acknowledge(&delivery).await.unwrap();
        let ComputeEnvelope::Request { spec } = delivery.envelope else {
            continue;
        };
        let binding = ComputeJobBinding {
            workspace_id: spec.workspace_id,
            job_id: spec.job_id,
            requester: spec.requester,
            worker: spec.worker,
        };
        worker
            .send(&ComputeEnvelope::Status {
                update: ComputeStatusUpdate {
                    binding: binding.clone(),
                    sequence: 1,
                    phase: ComputeJobPhase::Running,
                    log_tail: Vec::new(),
                },
            })
            .await
            .unwrap();
        let assignment = run_compute_assignment(
            Arc::clone(&executor),
            worker.connection(),
            spec,
            inputs.clone(),
            CaptureCancellation::default(),
        )
        .await;
        worker
            .send(&ComputeEnvelope::Result {
                result: ComputeResultUpdate {
                    binding,
                    sequence: 2,
                    phase: assignment.phase,
                    log_tail: assignment.log_tail,
                },
            })
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one invocation journey, local then offloaded, that reads top to bottom"
)]
async fn an_offloadable_action_runs_on_the_placed_device_and_returns_the_local_output() {
    let user = TestUser::fresh("offload owner");
    let relay_device = user.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let laptop_key = laptop.device_pubkey();
    let desktop = user.enrol("desktop", DeviceCapabilities::host());
    let desktop_key = desktop.device_pubkey();
    let relay = Box::pin(spawn_relay(
        &user,
        relay_device,
        &[WORKSPACE],
        &[laptop_key, desktop_key],
    ))
    .await;
    let address = relay.relay.node_addr();
    let root = tempfile::tempdir().unwrap();

    // The laptop: its workspace holds the photographs, it has the
    // thumbnails package, and it invokes the action.
    let store = Arc::new(fs::store::ContentStore::memory());
    let content = WorkspaceContent::new(Arc::clone(&store), tokio::runtime::Handle::current());
    let photographs: Vec<Vec<u8>> = (0..IMAGES)
        .map(|seed| photograph(u32::try_from(seed).unwrap()))
        .collect();
    let mut manifests = Vec::new();
    for bytes in &photographs {
        manifests.push(
            *store
                .write_content(bytes)
                .await
                .unwrap()
                .unwrap()
                .as_bytes(),
        );
    }
    let input_bytes: usize = photographs.iter().map(Vec::len).sum();
    let (laptop_package, package_digest) =
        installed_package(&root.path().join("laptop"), PACKAGE, ENTRYPOINT);
    let placement = Arc::new(RelayPlacement::new(
        relay_session(address.clone(), &user, &laptop).await,
        relay_key,
    ));
    let laptop_compute = compute_session(address.clone(), &user, relay_key, laptop).await;
    let offloader = Offloader::start(OffloaderParts {
        compute: laptop_compute,
        workspaces: vec![WORKSPACE],
        content: content.clone(),
        placement,
        packages: Arc::new(FixtureCatalogue(laptop_package)),
    })
    .await
    .unwrap();
    let action = make_thumbnails();
    let local: Arc<LocalTransform> =
        Arc::new(|inputs: &[Vec<u8>]| thumbnail_batch(inputs).map_err(|error| error.to_string()));
    let expected = thumbnail_batch(&photographs).unwrap();
    let expected_digests: Vec<[u8; 32]> = expected
        .iter()
        .map(|bytes| *blake3::hash(bytes).as_bytes())
        .collect();

    // No other device advertises compute: the action runs here.
    let here = tilo_test_env::within(
        "the local run",
        offloader.invoke(
            OffloadJob {
                workspace_id: WORKSPACE,
                action: &action,
                inputs: manifests.clone(),
            },
            Arc::clone(&local),
        ),
    )
    .await
    .unwrap();
    let RanOn::Local { reason } = &here.ran_on else {
        panic!("nothing is placed elsewhere yet: {:?}", here.ran_on);
    };
    assert!(reason.contains("no other device"), "{reason}");
    assert_eq!(here.outputs, expected_digests);
    assert_eq!(
        here.output.resources,
        expected_digests
            .iter()
            .map(|digest| tilo_actions::content_resource(&hex::encode(digest)).unwrap())
            .collect::<Vec<_>>()
    );

    // The desktop: the same signed package, the production executor, and
    // a compute advertisement for it.
    let (desktop_package, desktop_digest) =
        installed_package(&root.path().join("desktop"), PACKAGE, ENTRYPOINT);
    assert_eq!(desktop_digest, package_digest);
    let executor = Arc::new(ComputeWorker::new(
        worker_advertisement(desktop_key, WORKSPACE, PACKAGE),
        root.path().join("data"),
        root.path().join("execution"),
        FileComputeContentStore::open(root.path().join("retained")).unwrap(),
        FixtureCatalogue(desktop_package),
        ThumbnailSandbox,
    ));
    let mut desktop_compute = compute_session(address, &user, relay_key, desktop).await;
    desktop_compute.attach(WORKSPACE).await.unwrap();
    assert!(matches!(
        desktop_compute.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { .. }
    ));
    let issued_at_ms = unix_now_ms();
    desktop_compute
        .send(&ComputeEnvelope::Advertise {
            workspace_id: WORKSPACE,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker: desktop_key,
                workspace_id: WORKSPACE,
                apps: BTreeSet::from([PACKAGE.into()]),
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
    desktop_compute.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        desktop_compute.receive().await.unwrap().envelope,
        ComputeEnvelope::Pong
    ));
    let outputs: Arc<dyn ComputeOutputSource> = executor.clone();
    let desktop_connection = desktop_compute.connection();
    let serving = tokio::spawn(async move {
        while let Ok((mut send, mut receive)) = desktop_connection.accept_bi().await {
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
    let jobs = tokio::spawn(run_jobs(
        desktop_compute,
        Arc::clone(&executor),
        root.path().join("hydrated"),
    ));

    // Now the relay places the job on the desktop: it runs there, and the
    // laptop gets back exactly what it computed itself.
    let there = tilo_test_env::within(
        "the offloaded run",
        offloader.invoke(
            OffloadJob {
                workspace_id: WORKSPACE,
                action: &action,
                inputs: manifests.clone(),
            },
            Arc::clone(&local),
        ),
    )
    .await
    .unwrap();
    assert_eq!(there.ran_on, RanOn::Worker(desktop_key));
    assert_eq!(
        there.output, here.output,
        "the same output, wherever it ran"
    );
    assert_eq!(there.outputs, expected_digests);
    for (digest, bytes) in expected_digests.iter().zip(&expected) {
        assert_eq!(content.read(*digest).await.unwrap().as_ref(), Some(bytes));
        assert!(
            content.manifest_of(*digest).is_some(),
            "a workspace file version"
        );
    }
    let times = offloader.wall_times();
    assert!(times.iter().any(|time| time.device == laptop_key));
    assert!(times.iter().any(|time| time.device == desktop_key));
    eprintln!(
        "offloadable action ({IMAGES} images, {input_bytes} input bytes): \
         local {:?}, offloaded {:?} (single host: both devices share its cores)",
        here.elapsed, there.elapsed,
    );

    // An input that is not an image fails the batch on the worker, and the
    // action fails exactly as it does locally.
    let mut broken = manifests.clone();
    broken.insert(
        2,
        *store
            .write_content(b"not a photograph")
            .await
            .unwrap()
            .unwrap()
            .as_bytes(),
    );
    let refused = tilo_test_env::within(
        "a broken batch",
        offloader.invoke(
            OffloadJob {
                workspace_id: WORKSPACE,
                action: &action,
                inputs: broken,
            },
            Arc::clone(&local),
        ),
    )
    .await;
    assert_eq!(
        refused,
        Err(OffloadError::Action("input 2 is not an image".into()))
    );

    // Only workspace files the invoker resolved can be inputs.
    let unknown = offloader
        .invoke(
            OffloadJob {
                workspace_id: WORKSPACE,
                action: &action,
                inputs: vec![[0x5a; 32]],
            },
            Arc::clone(&local),
        )
        .await;
    assert!(matches!(unknown, Err(OffloadError::Input { index: 0, .. })));

    // An action that declares no offload is not taken down this path.
    let ordinary = ActionDescriptor {
        contract: ActionContract::view(),
        ..make_thumbnails()
    };
    assert!(matches!(
        offloader
            .invoke(
                OffloadJob {
                    workspace_id: WORKSPACE,
                    action: &ordinary,
                    inputs: manifests,
                },
                local,
            )
            .await,
        Err(OffloadError::NotOffloadable(_))
    ));
    jobs.abort();
    serving.abort();
}
