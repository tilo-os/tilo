//! An offloadable action — thumbnailing a batch of large images — invoked on
//! one device runs on another of its person's devices when the relay places
//! it there, hydrating its inputs from the invoker's workspace content and
//! returning its outputs by digest, and returns exactly what running it
//! locally returns. A device whose own replica already holds the inputs
//! runs the job on them once the invoker proves it holds the same bytes,
//! and the relay, having seen it do so, keeps placing such jobs there
//! however slow the link it measured. Through the action door's route, the
//! action Photos publishes runs there over the room's own media and answers
//! exactly what Photos' handler answers here.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until the test ends"
)]
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use fs::document_media::DocumentMedia;
use launcher::capture::{CaptureCancellation, CapturedRunOutput, CapturedTermination};
use runtime_fs::blobs::BlobScope;
use thumbnail::batch::{ENTRYPOINT, PACKAGE, thumbnail_batch};
use tilo_actions::offload::{ActionOffload, OffloadRequest, Offloaded, content_inputs};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, Offload,
    OperationKind,
};
use tilo_clock::unix_now_ms;
use tilo_relay::offload::{
    ComputePlacement, DeviceOffload, DeviceOffloadConfig, LocalTransform, OffloadError,
    OffloadIdentity, OffloadJob, OffloadRelay, OffloadRoute, Offloader, OffloaderParts,
    PlacementQuery, RanOn, RelayPlacement, WorkspaceContent, start_route,
};
use tilo_relay::{
    ComputeClient, ComputeEnvelope, ComputeJobBinding, ComputeJobPhase, ComputeOutputSource,
    ComputeResultUpdate, ComputeSandbox, ComputeSandboxInvocation, ComputeStatusUpdate,
    ComputeWorker, ComputeWorkerAdvertisement, Envelope, FileComputeContentStore, HeldContent,
    InputStaging, InputTransfer, PowerAvailability, RelayClient, WorkerEnvironments,
    WorkerHeldContent, run_compute_assignment, serve_worker_stream_with,
};
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
};

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

/// How the worker stages the next job's inputs, and how each job's were
/// staged.
struct Staging {
    next: Mutex<InputStaging>,
    staged: Mutex<Vec<InputTransfer>>,
}

/// Run every job routed to `worker`, as the production worker does: report
/// it running, hydrate and execute it, report its result.
async fn run_jobs(mut worker: ComputeClient, executor: Arc<Executor>, staging: Arc<Staging>) {
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
        let next = staging.next.lock().unwrap().clone();
        let assignment = run_compute_assignment(
            Arc::clone(&executor),
            worker.connection(),
            spec,
            next,
            CaptureCancellation::default(),
        )
        .await;
        staging.staged.lock().unwrap().push(assignment.transfer);
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
        placement: Arc::clone(&placement) as Arc<dyn ComputePlacement>,
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
    let staging = Arc::new(Staging {
        next: Mutex::new(InputStaging::from(root.path().join("hydrated"))),
        staged: Mutex::default(),
    });
    let jobs = tokio::spawn(run_jobs(
        desktop_compute,
        Arc::clone(&executor),
        Arc::clone(&staging),
    ));

    // The desktop's own replica of the workspace already holds the
    // photographs (it is the home server that keeps their originals): the
    // relay places the job there, the laptop proves it holds them too,
    // names their file versions, and no input byte moves. The laptop gets
    // back exactly what it computed itself.
    let desktop_store = Arc::new(fs::store::ContentStore::memory());
    for bytes in &photographs {
        desktop_store.write_content(bytes).await.unwrap().unwrap();
    }
    let desktop_content = WorkspaceContent::new(desktop_store, tokio::runtime::Handle::current());
    let held: Arc<dyn HeldContent> = Arc::new(desktop_content);
    staging.next.lock().unwrap().held = Some(held);
    let preseeded = tilo_test_env::within(
        "the offloaded run on held inputs",
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
    assert_eq!(preseeded.ran_on, RanOn::Worker(desktop_key));
    assert_eq!(
        preseeded.output, here.output,
        "the same output, whichever copy it ran on"
    );
    let from_held = *staging.staged.lock().unwrap().last().unwrap();
    assert_eq!(
        (from_held.inputs, from_held.bytes),
        (0, 0),
        "held inputs move no bytes"
    );
    assert_eq!(
        (from_held.held, from_held.held_bytes),
        (IMAGES, u64::try_from(input_bytes).unwrap())
    );

    // As deployed, the desktop is a home server: its filesystem replica
    // process keeps the workspace's store open, and the worker beside it
    // holds what the production worker holds for a job in this workspace —
    // its retained outputs and that replica, read from beside it.
    let replica_dir = root.path().join("full-replica");
    let replica = fs::store::ContentStore::open(&replica_dir).await.unwrap();
    for bytes in &photographs {
        replica.write_content(bytes).await.unwrap().unwrap();
    }
    let worker_held = WorkerHeldContent::new(
        Arc::new(FileComputeContentStore::open(root.path().join("retained")).unwrap()),
        &BTreeMap::from([(WORKSPACE, vec![replica_dir])]),
    );
    staging.next.lock().unwrap().held = Some(worker_held.for_workspace(WORKSPACE));
    let on_replica = tilo_test_env::within(
        "the offloaded run on a home server's replica",
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
    assert_eq!(on_replica.ran_on, RanOn::Worker(desktop_key));
    assert_eq!(
        on_replica.output, here.output,
        "the same output, run on the replica's copies"
    );
    let from_replica = *staging.staged.lock().unwrap().last().unwrap();
    assert_eq!(
        (from_replica.inputs, from_replica.bytes),
        (0, 0),
        "inputs a replica holds move no bytes"
    );
    assert_eq!(
        (from_replica.held, from_replica.held_bytes),
        (IMAGES, u64::try_from(input_bytes).unwrap())
    );
    // A job in another workspace never reaches this replica.
    assert!(!worker_held.for_workspace(WORKSPACE + 1).locates());

    // Photographs the desktop does not hold go to it next: it held all of
    // its last job's inputs, so the relay expects nothing to move. It
    // finds no copy and fetches them, and the relay measures the link
    // while forwarding them.
    let others: Vec<Vec<u8>> = (100..103).map(photograph).collect();
    let mut other_manifests = Vec::new();
    for bytes in &others {
        other_manifests.push(
            *store
                .write_content(bytes)
                .await
                .unwrap()
                .unwrap()
                .as_bytes(),
        );
    }
    let other_bytes: u64 = others
        .iter()
        .map(|bytes| u64::try_from(bytes.len()).unwrap())
        .sum();
    let elsewhere = tilo_test_env::within(
        "the offloaded run on photographs the desktop lacks",
        offloader.invoke(
            OffloadJob {
                workspace_id: WORKSPACE,
                action: &action,
                inputs: other_manifests,
            },
            Arc::clone(&local),
        ),
    )
    .await
    .unwrap();
    assert_eq!(elsewhere.ran_on, RanOn::Worker(desktop_key));
    assert_eq!(
        elsewhere.outputs,
        thumbnail_batch(&others)
            .unwrap()
            .iter()
            .map(|bytes| *blake3::hash(bytes).as_bytes())
            .collect::<Vec<_>>()
    );
    let lacked = *staging.staged.lock().unwrap().last().unwrap();
    assert_eq!(
        (lacked.inputs, lacked.bytes, lacked.held),
        (others.len(), other_bytes, 0)
    );

    // Now the link is measured. Weighed against a 250 ms run, more than any
    // loopback round trip and less than moving 129 MB through the relay
    // takes, the batch the desktop was seen to hold moves nothing and goes
    // to it, while the same bytes named by size alone, as placement saw
    // every batch before, stay on the laptop. Photographs the desktop
    // fetched are weighed in full: against a 1 ms run they stay too.
    let quick_run_ms = 250;
    let sized = |photos: &[Vec<u8>]| -> Vec<([u8; 32], u64)> {
        photos
            .iter()
            .map(|bytes| {
                (
                    *blake3::hash(bytes).as_bytes(),
                    u64::try_from(bytes.len()).unwrap(),
                )
            })
            .collect()
    };
    let (held_inputs, lacked_inputs) = (sized(&photographs), sized(&others));
    let input_total = u64::try_from(input_bytes).unwrap();
    let ask =
        |what: &'static str, listed: Vec<([u8; 32], u64)>, input_bytes: u64, local_wall_ms: u64| {
            let placement = Arc::clone(&placement);
            async move {
                let placed = placement
                    .place(PlacementQuery {
                        workspace_id: WORKSPACE,
                        app: PACKAGE,
                        gpu: false,
                        input_bytes,
                        inputs: &listed,
                        local_wall_ms: Some(local_wall_ms),
                    })
                    .await
                    .unwrap();
                eprintln!("placing {what} against a {local_wall_ms} ms local run: {placed:?}");
                placed
            }
        };
    let to_holder = ask(
        "the held batch",
        held_inputs.clone(),
        input_total,
        quick_run_ms,
    )
    .await;
    assert_eq!(to_holder.worker, Some(desktop_key), "{}", to_holder.reason);
    let by_size = ask(
        "the batch by size alone",
        Vec::new(),
        input_total,
        quick_run_ms,
    )
    .await;
    assert_eq!(by_size.worker, None, "{}", by_size.reason);
    assert!(
        by_size.reason.starts_with("inputs_heavier_than_gain"),
        "{}",
        by_size.reason
    );
    let kept_here = ask("the fetched photographs", lacked_inputs, other_bytes, 1).await;
    assert_eq!(kept_here.worker, None, "{}", kept_here.reason);
    // What placement weighed before it knew what the desktop holds: every
    // byte of the batch, against the laptop's own run.
    let local_ms = u64::try_from(here.elapsed.as_millis()).unwrap();
    ask("the batch by size alone", Vec::new(), input_total, local_ms).await;

    // So the batch the desktop holds goes to it however slow the link,
    // and moves no input byte.
    let again = tilo_test_env::within(
        "the offloaded run on held inputs over a measured link",
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
    assert_eq!(again.ran_on, RanOn::Worker(desktop_key));
    assert_eq!(again.output, here.output);
    let held_again = *staging.staged.lock().unwrap().last().unwrap();
    assert_eq!(
        (held_again.inputs, held_again.bytes, held_again.held),
        (0, 0, IMAGES),
        "held inputs move no bytes"
    );

    // Without a replica the relay still expects the desktop to hold the
    // photographs, so the job goes there; the inputs hydrate from the
    // laptop through the relay, and the output is still the same. A wrong
    // hint costs time, never the output, and this run unlearns it.
    staging.next.lock().unwrap().held = None;
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
    let fetched = *staging.staged.lock().unwrap().last().unwrap();
    assert_eq!(fetched.bytes, u64::try_from(input_bytes).unwrap());
    assert_eq!(fetched.held, 0);
    let unlearned = ask(
        "the batch it fetched again",
        held_inputs,
        input_total,
        quick_run_ms,
    )
    .await;
    assert_eq!(unlearned.worker, None, "{}", unlearned.reason);
    let times = offloader.wall_times();
    assert!(times.iter().any(|time| time.device == laptop_key));
    assert!(times.iter().any(|time| time.device == desktop_key));
    assert!(
        times.iter().all(|time| time.input_bytes == input_total),
        "{times:?}"
    );
    eprintln!(
        "offloadable action ({IMAGES} images, {input_bytes} input bytes): \
         local {:?}, offloaded {:?} (inputs staged in {:?}), \
         offloaded to a worker already holding them {:?} (staged in {:?}), \
         offloaded to a home server's replica {:?} (staged in {:?}), \
         and again once the link was measured {:?} (staged in {:?}) \
         (single host: both devices share its cores)",
        here.elapsed,
        there.elapsed,
        fetched.elapsed,
        preseeded.elapsed,
        from_held.elapsed,
        on_replica.elapsed,
        from_replica.elapsed,
        again.elapsed,
        held_again.elapsed,
    );
    drop(replica);

    // The relay measured the link while it moved those inputs, so it now
    // weighs moving them against the laptop's own run: inputs far heavier
    // than any gain stay on the laptop, with the reason; a light job of
    // heavy compute still goes to the desktop.
    let heavy = placement
        .place(PlacementQuery {
            workspace_id: WORKSPACE,
            app: PACKAGE,
            gpu: false,
            input_bytes: 1 << 40,
            inputs: &[],
            local_wall_ms: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(heavy.worker, None, "{}", heavy.reason);
    assert!(
        heavy.reason.starts_with("inputs_heavier_than_gain"),
        "{}",
        heavy.reason
    );
    let light = placement
        .place(PlacementQuery {
            workspace_id: WORKSPACE,
            app: PACKAGE,
            gpu: false,
            input_bytes: 1,
            inputs: &[],
            local_wall_ms: Some(600_000),
        })
        .await
        .unwrap();
    assert_eq!(light.worker, Some(desktop_key), "{}", light.reason);

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

/// Attach `worker` and advertise the thumbnails package from it.
async fn advertise_thumbnails(worker: &mut ComputeClient, device: [u8; 32]) {
    worker.attach(WORKSPACE).await.unwrap();
    assert!(matches!(
        worker.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { .. }
    ));
    let issued_at_ms = unix_now_ms();
    worker
        .send(&ComputeEnvelope::Advertise {
            workspace_id: WORKSPACE,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker: device,
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
    worker.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        worker.receive().await.unwrap().envelope,
        ComputeEnvelope::Pong
    ));
}

/// Take the next job routed to `worker` and report it running, then
/// never finish it.
async fn take_one_job(worker: &mut ComputeClient) {
    loop {
        let delivery = worker.receive().await.unwrap();
        worker.acknowledge(&delivery).await.unwrap();
        let ComputeEnvelope::Request { spec } = delivery.envelope else {
            continue;
        };
        worker
            .send(&ComputeEnvelope::Status {
                update: ComputeStatusUpdate {
                    binding: ComputeJobBinding {
                        workspace_id: spec.workspace_id,
                        job_id: spec.job_id,
                        requester: spec.requester,
                        worker: spec.worker,
                    },
                    sequence: 1,
                    phase: ComputeJobPhase::Running,
                    log_tail: Vec::new(),
                },
            })
            .await
            .unwrap();
        return;
    }
}

/// How long after its worker went away an invocation may take to come back
/// with the local output: the local run of a two-image batch and a margin,
/// far inside the job's lifetime (four and a half minutes here), which the
/// requester used to wait out.
const PROMPT_FALLBACK: std::time::Duration = std::time::Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "two workers going away mid-job, lost then revoked, read top to bottom"
)]
async fn a_lost_or_revoked_worker_fails_its_job_at_once_and_the_action_runs_locally() {
    let user = TestUser::fresh("offload owner");
    let relay_device = user.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let laptop_key = laptop.device_pubkey();
    let desktop = user.enrol("desktop", DeviceCapabilities::host());
    let desktop_key = desktop.device_pubkey();
    let server = user.enrol("server", DeviceCapabilities::host());
    let server_key = server.device_pubkey();
    let relay = Box::pin(spawn_relay(
        &user,
        relay_device,
        &[WORKSPACE],
        &[laptop_key, desktop_key, server_key],
    ))
    .await;
    let address = relay.relay.node_addr();
    let root = tempfile::tempdir().unwrap();

    let store = Arc::new(fs::store::ContentStore::memory());
    let content = WorkspaceContent::new(Arc::clone(&store), tokio::runtime::Handle::current());
    let photographs: Vec<Vec<u8>> = (0..2).map(photograph).collect();
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
    let (laptop_package, _) = installed_package(&root.path().join("laptop"), PACKAGE, ENTRYPOINT);
    let placement = Arc::new(RelayPlacement::new(
        relay_session(address.clone(), &user, &laptop).await,
        relay_key,
    ));
    let offloader = Offloader::start(OffloaderParts {
        compute: compute_session(address.clone(), &user, relay_key, laptop).await,
        workspaces: vec![WORKSPACE],
        content,
        placement,
        packages: Arc::new(FixtureCatalogue(laptop_package)),
    })
    .await
    .unwrap();
    let action = make_thumbnails();
    let local: Arc<LocalTransform> =
        Arc::new(|inputs: &[Vec<u8>]| thumbnail_batch(inputs).map_err(|error| error.to_string()));
    let expected: Vec<[u8; 32]> = thumbnail_batch(&photographs)
        .unwrap()
        .iter()
        .map(|bytes| *blake3::hash(bytes).as_bytes())
        .collect();
    let invoke = || {
        offloader.invoke(
            OffloadJob {
                workspace_id: WORKSPACE,
                action: &action,
                inputs: manifests.clone(),
            },
            Arc::clone(&local),
        )
    };

    // The desktop takes the job, reports it running, and its connection
    // drops: the laptop hears `worker_lost` and runs the action itself.
    let mut desktop_compute = compute_session(address.clone(), &user, relay_key, desktop).await;
    advertise_thumbnails(&mut desktop_compute, desktop_key).await;
    let (lost, gone_at) = tilo_test_env::within("the run after a lost worker", async {
        tokio::join!(invoke(), async {
            take_one_job(&mut desktop_compute).await;
            desktop_compute.close().await;
            std::time::Instant::now()
        })
    })
    .await;
    let lost = lost.unwrap();
    let after_loss = gone_at.elapsed();
    let RanOn::Local { reason } = &lost.ran_on else {
        panic!("the lost worker's job runs locally: {:?}", lost.ran_on);
    };
    assert!(reason.contains("worker_lost"), "{reason}");
    assert_eq!(lost.outputs, expected, "the local output, in full");
    assert!(
        after_loss < PROMPT_FALLBACK,
        "the laptop fell back {after_loss:?} after the worker left"
    );

    // The server takes the next job, reports it running, and its device
    // certificate is revoked: the relay ends its session and the laptop
    // hears `worker_revoked`.
    let mut server_compute = compute_session(address, &user, relay_key, server).await;
    advertise_thumbnails(&mut server_compute, server_key).await;
    let (revoked, revoked_at) = tilo_test_env::within("the run after a revoked worker", async {
        tokio::join!(invoke(), async {
            take_one_job(&mut server_compute).await;
            let mut entry = RevocationEntry {
                user_public_key: user.user_pk,
                revoked_device_public_key: server_key,
                revoked_at_ns: 1,
                reason: RevocationReason::Stolen,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, &user.user_sk).unwrap();
            tilo_relay::store::save_revocations(&relay.state_root, &[entry]).unwrap();
            std::time::Instant::now()
        })
    })
    .await;
    let revoked = revoked.unwrap();
    let after_revocation = revoked_at.elapsed();
    let RanOn::Local { reason } = &revoked.ran_on else {
        panic!(
            "the revoked worker's job runs locally: {:?}",
            revoked.ran_on
        );
    };
    assert!(reason.contains("worker_revoked"), "{reason}");
    assert_eq!(revoked.outputs, expected);
    assert!(
        after_revocation < PROMPT_FALLBACK,
        "the laptop fell back {after_revocation:?} after the revocation"
    );
    // The revoked worker is told and cut off.
    let cut_off = tilo_test_env::within("the revoked worker's session to end", async {
        loop {
            match server_compute.receive().await {
                Ok(delivery) => {
                    if let ComputeEnvelope::NotAuthorised { reason, .. } = delivery.envelope {
                        break Some(reason);
                    }
                }
                Err(_) => break None,
            }
        }
    })
    .await;
    if let Some(reason) = cut_off {
        assert!(reason.contains("revoked"), "{reason}");
    }
}

/// The room the door's offload journey runs in, and the app invoked there.
const ROOM: &str = "0b5c7a7e-6f43-4a39-9d1c-1d3f2a6c0e11";
const PHOTOS: &str = "so.tilo.photos";

/// Photos' published `make-thumbnails` and a request for it over `hashes`,
/// as the door builds one from an admitted invocation's arguments.
fn make_thumbnails_request(hashes: &[String]) -> OffloadRequest {
    use tilo_resource::Effect as _;
    let spec = photos::actions::PhotosEffect::specs()
        .into_iter()
        .find(|spec| spec.id == "make-thumbnails")
        .expect("Photos publishes make-thumbnails");
    let arguments = serde_json::json!({ "photos": hashes });
    OffloadRequest {
        room: ROOM.into(),
        app_id: PHOTOS.into(),
        address: ActionAddress::new("photos.library:main", "make-thumbnails"),
        offload: spec.contract.offload.clone().expect("declared offloadable"),
        inputs: content_inputs(spec.params(), arguments.as_object().unwrap()).unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one door journey, here then on a worker, that reads top to bottom"
)]
async fn the_door_runs_photos_make_thumbnails_on_the_placed_device_with_the_local_output() {
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

    // The laptop: Photos put the photographs in the room's document media,
    // and its route places the room's compute in the relay workspace.
    let store = Arc::new(fs::store::ContentStore::memory());
    let media = DocumentMedia::new(Arc::clone(&store));
    let scope = BlobScope::new(ROOM, PHOTOS).unwrap();
    let photographs: Vec<Vec<u8>> = (0..IMAGES)
        .map(|seed| photograph(u32::try_from(seed).unwrap()))
        .collect();
    let mut hashes = Vec::new();
    for bytes in &photographs {
        let hash = media.store(&scope, bytes.clone()).await.unwrap();
        hashes.push(hash.to_hex());
    }
    let (laptop_package, _) = installed_package(&root.path().join("laptop"), PACKAGE, ENTRYPOINT);
    let placement = Arc::new(RelayPlacement::new(
        relay_session(address.clone(), &user, &laptop).await,
        relay_key,
    ));
    let offloader = Arc::new(
        Offloader::start(OffloaderParts {
            compute: compute_session(address.clone(), &user, relay_key, laptop).await,
            workspaces: vec![WORKSPACE],
            content: WorkspaceContent::new(Arc::clone(&store), tokio::runtime::Handle::current()),
            placement,
            packages: Arc::new(FixtureCatalogue(laptop_package)),
        })
        .await
        .unwrap(),
    );
    let route = OffloadRoute::new(
        Arc::clone(&offloader),
        Arc::clone(&media),
        std::collections::HashMap::from([(ROOM.to_owned(), WORKSPACE)]),
    );
    // An agent asks for thumbnails of every photograph, one named twice.
    let mut named = hashes.clone();
    named.push(hashes[0].clone());
    let request = make_thumbnails_request(&named);
    assert_eq!(request.inputs.len(), IMAGES + 1);

    // No other device advertises compute: the door runs Photos' handler
    // here, and reports how long it took.
    let Offloaded::RunHere { reason } = route.offload(request.clone()).await else {
        panic!("nothing is placed elsewhere yet");
    };
    assert!(reason.contains("no other device"), "{reason}");
    let app_content = tilo_content::ContentStore::memory();
    for bytes in &photographs {
        app_content.put(bytes).unwrap();
    }
    let started = std::time::Instant::now();
    let here = photos::actions::make_thumbnails(&named, &app_content).unwrap();
    let local_elapsed = started.elapsed();
    route.ran_here(&request, local_elapsed);
    assert_eq!(here.resources.len(), IMAGES, "one thumbnail per photograph");
    assert!(
        offloader
            .wall_times()
            .iter()
            .any(|time| time.device == laptop_key && time.elapsed == local_elapsed),
        "the local run is what placement weighs next"
    );

    // The desktop advertises the thumbnails package and runs its jobs.
    let (desktop_package, _) = installed_package(&root.path().join("desktop"), PACKAGE, ENTRYPOINT);
    let executor = Arc::new(ComputeWorker::new(
        worker_advertisement(desktop_key, WORKSPACE, PACKAGE),
        root.path().join("data"),
        root.path().join("execution"),
        FileComputeContentStore::open(root.path().join("retained")).unwrap(),
        FixtureCatalogue(desktop_package),
        ThumbnailSandbox,
    ));
    let mut desktop_compute = compute_session(address, &user, relay_key, desktop).await;
    advertise_thumbnails(&mut desktop_compute, desktop_key).await;
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
    let staging = Arc::new(Staging {
        next: Mutex::new(InputStaging::from(root.path().join("hydrated"))),
        staged: Mutex::default(),
    });
    let jobs = tokio::spawn(run_jobs(
        desktop_compute,
        Arc::clone(&executor),
        Arc::clone(&staging),
    ));

    // The same invocation now runs on the desktop, and the door answers it
    // with exactly what Photos answered here; the thumbnails are the room's
    // media, readable by the addresses in the output.
    let door_run =
        tilo_test_env::within("the offloaded door run", route.offload(request.clone())).await;
    let Offloaded::Ran {
        output,
        device,
        elapsed: remote_elapsed,
    } = door_run
    else {
        panic!("the desktop is better placed: {door_run:?}");
    };
    assert_eq!(device, hex::encode(desktop_key));
    assert_eq!(output, here, "the same output, wherever it ran");
    for resource in &output.resources {
        let hash = tilo_actions::content_hash(resource).unwrap();
        let mut digest = [0; 32];
        hex::decode_to_slice(hash, &mut digest).unwrap();
        assert!(
            media.contains(&scope, digest.into()).await,
            "the room's media"
        );
        assert_eq!(
            app_content.read(hash),
            store
                .read_blob_range(digest.into(), 0..u64::MAX)
                .await
                .map(|(bytes, _)| bytes),
            "byte for byte the local thumbnail"
        );
    }
    let fetched = *staging.staged.lock().unwrap().last().unwrap();
    assert_eq!(fetched.inputs, IMAGES, "each distinct input staged once");
    eprintln!(
        "make-thumbnails through the door ({IMAGES} images, {} input bytes): \
         Photos' handler here {local_elapsed:?}, offloaded {remote_elapsed:?} \
         (inputs staged in {:?}; single host: both devices share its cores)",
        photographs.iter().map(Vec::len).sum::<usize>(),
        fetched.elapsed,
    );

    // Content another room put on this device is never an input: the door
    // runs the app's handler, which cannot read it either.
    let elsewhere = BlobScope::new("another-room", PHOTOS).unwrap();
    let foreign = media
        .store(&elsewhere, b"another room's bytes".to_vec())
        .await
        .unwrap()
        .to_hex();
    let Offloaded::RunHere { reason } = route.offload(make_thumbnails_request(&[foreign])).await
    else {
        panic!("another room's content stays here");
    };
    assert!(reason.contains("not the room's content"), "{reason}");

    // A room this device places no compute for runs here.
    let mut unplaced = request;
    unplaced.room = "a-room-without-compute".into();
    let Offloaded::RunHere { reason } = route.offload(unplaced).await else {
        panic!("no workspace, no offload");
    };
    assert!(reason.contains("no compute for the room"), "{reason}");
    jobs.abort();
    serving.abort();
}

/// `device`'s identity as its session starts its offload route from.
fn offload_identity(user: &TestUser, device: &TestDevice) -> OffloadIdentity {
    OffloadIdentity {
        user_public_key: user.user_pk,
        device_cert: device.cert.clone(),
        device_signing_key: device.sk.clone(),
        revocations: RevocationList::new(),
    }
}

/// Wait until `offload` has started or failed to.
async fn settled(offload: &DeviceOffload) {
    tilo_test_env::within("the offload route's start", async {
        while offload.started().is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one session's route, configured, placed elsewhere, then refused, read top to bottom"
)]
async fn a_session_route_started_from_its_home_relay_offloads_and_falls_back_truthfully() {
    let user = TestUser::fresh("offload owner");
    let relay_device = user.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let laptop_key = laptop.device_pubkey();
    let desktop = user.enrol("desktop", DeviceCapabilities::host());
    let desktop_key = desktop.device_pubkey();
    // Another person's device, which the relay never lets in.
    let someone_else = TestUser::fresh("someone else");
    let stranger = someone_else.enrol("stranger", DeviceCapabilities::primary());
    let relay = Box::pin(spawn_relay(
        &user,
        relay_device,
        &[WORKSPACE],
        &[laptop_key, desktop_key],
    ))
    .await;
    let address = relay.relay.node_addr();
    let root = tempfile::tempdir().unwrap();

    // The desktop advertises the thumbnails package and runs its jobs.
    let (desktop_package, _) = installed_package(&root.path().join("desktop"), PACKAGE, ENTRYPOINT);
    let executor = Arc::new(ComputeWorker::new(
        worker_advertisement(desktop_key, WORKSPACE, PACKAGE),
        root.path().join("data"),
        root.path().join("execution"),
        FileComputeContentStore::open(root.path().join("retained")).unwrap(),
        FixtureCatalogue(desktop_package),
        ThumbnailSandbox,
    ));
    let mut desktop_compute = compute_session(address.clone(), &user, relay_key, desktop).await;
    advertise_thumbnails(&mut desktop_compute, desktop_key).await;
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
    let staging = Arc::new(Staging {
        next: Mutex::new(InputStaging::from(root.path().join("hydrated"))),
        staged: Mutex::default(),
    });
    let jobs = tokio::spawn(run_jobs(desktop_compute, Arc::clone(&executor), staging));

    // The laptop's session: Photos put the photographs in the room's media,
    // and the session starts its route from the registered home relay that
    // retains the room, handing it to the broker before it is up.
    let store = Arc::new(fs::store::ContentStore::memory());
    let media = DocumentMedia::new(Arc::clone(&store));
    let scope = BlobScope::new(ROOM, PHOTOS).unwrap();
    let photographs: Vec<Vec<u8>> = (0..3).map(photograph).collect();
    let mut hashes = Vec::new();
    for bytes in &photographs {
        hashes.push(media.store(&scope, bytes.clone()).await.unwrap().to_hex());
    }
    let (laptop_package, _) = installed_package(&root.path().join("laptop"), PACKAGE, ENTRYPOINT);
    let config = DeviceOffloadConfig {
        relay: OffloadRelay {
            address: address.clone(),
            device: relay_key,
        },
        identity: offload_identity(&user, &laptop),
        rooms: std::collections::HashMap::from([(ROOM.to_owned(), WORKSPACE)]),
    };
    let route = DeviceOffload::spawn(
        start_route(
            config.clone(),
            client_endpoint().await,
            Arc::clone(&store),
            Arc::new(FixtureCatalogue(laptop_package)),
        ),
        &tokio::runtime::Handle::current(),
    );
    settled(&route).await;
    assert_eq!(
        route.started(),
        Some(Ok(())),
        "the home relay admits the laptop"
    );

    // An agent asks for the thumbnails: they are made on the desktop, and
    // the door gets exactly what Photos' handler makes here, kept as the
    // room's media.
    let request = make_thumbnails_request(&hashes);
    let app_content = tilo_content::ContentStore::memory();
    for bytes in &photographs {
        app_content.put(bytes).unwrap();
    }
    let here = photos::actions::make_thumbnails(&hashes, &app_content).unwrap();
    let ran = tilo_test_env::within("the offloaded run", route.offload(request.clone())).await;
    let Offloaded::Ran { output, device, .. } = ran else {
        panic!("the desktop is better placed: {ran:?}");
    };
    assert_eq!(device, hex::encode(desktop_key));
    assert_eq!(output, here, "the same output, wherever it ran");
    for resource in &output.resources {
        let hash = tilo_actions::content_hash(resource).unwrap();
        let mut digest = [0; 32];
        hex::decode_to_slice(hash, &mut digest).unwrap();
        assert!(
            media.contains(&scope, digest.into()).await,
            "the room's media"
        );
        assert_eq!(
            app_content.read(hash),
            store
                .read_blob_range(digest.into(), 0..u64::MAX)
                .await
                .map(|(bytes, _)| bytes),
            "byte for byte the local thumbnail"
        );
    }

    // A room the relay does not place stays here.
    let mut unplaced = request.clone();
    unplaced.room = "a-room-without-compute".into();
    let Offloaded::RunHere { reason } = route.offload(unplaced).await else {
        panic!("no workspace, no offload");
    };
    assert!(reason.contains("no compute for the room"), "{reason}");

    // A device the relay refuses never gets a route up: every offloadable
    // invocation runs here, and the door is told why.
    let (stranger_package, _) =
        installed_package(&root.path().join("stranger"), PACKAGE, ENTRYPOINT);
    let refused = DeviceOffload::spawn(
        start_route(
            DeviceOffloadConfig {
                identity: offload_identity(&someone_else, &stranger),
                ..config
            },
            client_endpoint().await,
            Arc::clone(&store),
            Arc::new(FixtureCatalogue(stranger_package)),
        ),
        &tokio::runtime::Handle::current(),
    );
    settled(&refused).await;
    assert!(
        matches!(refused.started(), Some(Err(_))),
        "{:?}",
        refused.started()
    );
    let Offloaded::RunHere { reason } = refused.offload(request).await else {
        panic!("a route that did not start places nothing elsewhere");
    };
    assert!(reason.contains("did not start"), "{reason}");
    jobs.abort();
    serving.abort();
}
