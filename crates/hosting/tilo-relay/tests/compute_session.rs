#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use std::collections::BTreeSet;

use tilo_clock::unix_now_ms;
use tilo_relay::{
    ComputeClient, ComputeEnvelope, ComputeJobBinding, ComputeJobPhase, ComputeJobSpec,
    ComputeResourceLimits, ComputeResultUpdate, ComputeStatusUpdate, ComputeWorkerAdvertisement,
    PowerAvailability,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint, spawn_relay, spawn_relay_on};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end session scenario that reads top to bottom"
)]
async fn compute_session_routes_and_replays_unacknowledged_status() {
    let user = TestUser::fresh("compute owner");
    let relay_device = user.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let requester = user.enrol("requester", DeviceCapabilities::primary());
    let requester_key = requester.device_pubkey();
    let worker = user.enrol("worker", DeviceCapabilities::host());
    let worker_key = worker.device_pubkey();
    let relay = Box::pin(spawn_relay(
        &user,
        relay_device,
        &[77],
        &[requester_key, worker_key],
    ))
    .await;
    let relay_address = relay.relay.node_addr();

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
    worker_client.attach(77).await.unwrap();
    assert!(matches!(
        worker_client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready {
            workspace_id: 77,
            pending: 0
        }
    ));
    let issued_at_ms = unix_now_ms();
    worker_client
        .send(&ComputeEnvelope::Advertise {
            workspace_id: 77,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker: worker_key,
                workspace_id: 77,
                apps: BTreeSet::from(["org.tilo.render".into()]),
                accelerators: BTreeSet::new(),
                assignment_capacity: 1,
                active_assignments: 0,
                environment_capacity: 0,
                active_environments: 0,
                power: PowerAvailability::External,
                energy_saver: false,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(30_000),
            },
        })
        .await
        .unwrap();

    let requester_cert = requester.cert.clone();
    let requester_keypair = requester.sk.clone();
    let mut requester_client = ComputeClient::connect(
        client_endpoint().await,
        relay_address.clone(),
        user.user_pk,
        Some(relay_key),
        requester.cert,
        requester.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    requester_client.attach(77).await.unwrap();
    assert!(matches!(
        requester_client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { pending: 0, .. }
    ));
    let specification = ComputeJobSpec {
        version: 1,
        job_id: [8; 16],
        requester: requester_key,
        worker: worker_key,
        workspace_id: 77,
        app_id: "org.tilo.render".into(),
        package_digest: [7; 32],
        entrypoint: "render".into(),
        arguments: Vec::new(),
        inputs: vec![[6; 32]],
        limits: ComputeResourceLimits {
            cpu_time_ms: 5_000,
            memory_bytes: 32 * 1024 * 1024,
            output_bytes: 1024,
            gpu: false,
        },
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(30_000),
    };
    requester_client
        .send(&ComputeEnvelope::Request {
            spec: specification.clone(),
        })
        .await
        .unwrap();
    let request_delivery =
        tilo_test_env::within("a message on worker_client", worker_client.receive())
            .await
            .unwrap();
    assert!(matches!(
        &request_delivery.envelope,
        ComputeEnvelope::Request { spec } if spec == &specification
    ));
    worker_client.acknowledge(&request_delivery).await.unwrap();

    drop(requester_client);
    let binding = ComputeJobBinding {
        workspace_id: 77,
        job_id: specification.job_id,
        requester: requester_key,
        worker: worker_key,
    };
    worker_client
        .send(&ComputeEnvelope::Status {
            update: ComputeStatusUpdate {
                binding,
                sequence: 1,
                phase: ComputeJobPhase::Running,
                log_tail: b"started".to_vec(),
            },
        })
        .await
        .unwrap();

    // Sending a frame does not establish processing order with a new
    // connection. Fence the status on this ordered stream before reconnecting,
    // as the offline-cancel scenario does for its durable enqueue.
    worker_client.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        tilo_test_env::within(
            "relay processed status before reconnect",
            worker_client.receive()
        )
        .await
        .unwrap()
        .envelope,
        ComputeEnvelope::Pong
    ));

    let mut requester_again = ComputeClient::connect(
        client_endpoint().await,
        relay_address,
        user.user_pk,
        Some(relay_key),
        requester_cert,
        requester_keypair,
        RevocationList::new(),
    )
    .await
    .unwrap();
    requester_again.attach(77).await.unwrap();
    let replayed = tilo_test_env::within("a message on requester_again", requester_again.receive())
        .await
        .unwrap();
    assert!(matches!(
        replayed.envelope,
        ComputeEnvelope::Status {
            update: ComputeStatusUpdate {
                sequence: 1,
                phase: ComputeJobPhase::Running,
                ..
            }
        }
    ));
    requester_again.acknowledge(&replayed).await.unwrap();
    assert!(matches!(
        requester_again.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready {
            workspace_id: 77,
            pending: 1
        }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end session scenario that reads top to bottom"
)]
/// A worker that drops its connection while it holds a job does not leave
/// the job hanging: the relay ends it as `worker_lost` and tells the
/// requester at once, durably, replaying the result until the requester
/// acknowledges it. The job is over, so the requester's late cancel goes
/// nowhere, and the returning worker is handed nothing stale.
async fn compute_session_ends_a_lost_workers_job_and_replays_it_until_ack() {
    let user = TestUser::fresh("compute cancel owner");
    let relay_device = user.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let requester = user.enrol("requester", DeviceCapabilities::primary());
    let requester_key = requester.device_pubkey();
    let worker = user.enrol("worker", DeviceCapabilities::host());
    let worker_key = worker.device_pubkey();
    let worker_cert = worker.cert.clone();
    let worker_keypair = worker.sk.clone();
    let relay = Box::pin(spawn_relay(
        &user,
        relay_device,
        &[88],
        &[requester_key, worker_key],
    ))
    .await;
    let relay_address = relay.relay.node_addr();

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
    worker_client.attach(88).await.unwrap();
    assert!(matches!(
        worker_client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready {
            workspace_id: 88,
            pending: 0
        }
    ));
    let issued_at_ms = unix_now_ms();
    worker_client
        .send(&ComputeEnvelope::Advertise {
            workspace_id: 88,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker: worker_key,
                workspace_id: 88,
                apps: BTreeSet::from(["org.tilo.render".into()]),
                accelerators: BTreeSet::new(),
                assignment_capacity: 1,
                active_assignments: 0,
                environment_capacity: 0,
                active_environments: 0,
                power: PowerAvailability::External,
                energy_saver: false,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(30_000),
            },
        })
        .await
        .unwrap();

    let mut requester_client = ComputeClient::connect(
        client_endpoint().await,
        relay_address.clone(),
        user.user_pk,
        Some(relay_key),
        requester.cert.clone(),
        requester.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    requester_client.attach(88).await.unwrap();
    assert!(matches!(
        requester_client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { pending: 0, .. }
    ));
    let specification = ComputeJobSpec {
        version: 1,
        job_id: [18; 16],
        requester: requester_key,
        worker: worker_key,
        workspace_id: 88,
        app_id: "org.tilo.render".into(),
        package_digest: [17; 32],
        entrypoint: "render".into(),
        arguments: Vec::new(),
        inputs: Vec::new(),
        limits: ComputeResourceLimits {
            cpu_time_ms: 5_000,
            memory_bytes: 32 * 1024 * 1024,
            output_bytes: 1024,
            gpu: false,
        },
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(30_000),
    };
    requester_client.submit(&specification).await.unwrap();
    let request_delivery =
        tilo_test_env::within("a message on worker_client", worker_client.receive())
            .await
            .unwrap();
    assert!(matches!(
        &request_delivery.envelope,
        ComputeEnvelope::Request { spec } if spec == &specification
    ));
    worker_client.acknowledge(&request_delivery).await.unwrap();
    worker_client.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        tilo_test_env::within("a message on worker_client", worker_client.receive())
            .await
            .unwrap()
            .envelope,
        ComputeEnvelope::Pong
    ));
    drop(worker_client);

    // The worker is gone holding the job: the requester hears at once.
    let binding = ComputeJobBinding {
        workspace_id: 88,
        job_id: specification.job_id,
        requester: requester_key,
        worker: worker_key,
    };
    let lost = ComputeJobPhase::Failed {
        code: "worker_lost".into(),
    };
    let told = tilo_test_env::within("the lost worker's result", requester_client.receive())
        .await
        .unwrap();
    let durable_sequence = told.sequence.expect("the result must be durable");
    assert!(matches!(
        &told.envelope,
        ComputeEnvelope::Result { result }
            if result.binding == binding && result.phase == lost && result.sequence == 1
    ));
    // Cancelling an ended job routes nothing.
    requester_client.cancel(&binding).await.unwrap();
    requester_client.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        tilo_test_env::within("a message on requester_client", requester_client.receive())
            .await
            .unwrap()
            .envelope,
        ComputeEnvelope::Pong
    ));
    drop(requester_client);

    // Unacknowledged, the result is replayed to the requester's next
    // session, and gone once acknowledged.
    let mut requester_again = ComputeClient::connect(
        client_endpoint().await,
        relay_address.clone(),
        user.user_pk,
        Some(relay_key),
        requester.cert.clone(),
        requester.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    requester_again.attach(88).await.unwrap();
    let replayed = tilo_test_env::within("the replayed result", requester_again.receive())
        .await
        .unwrap();
    assert_eq!(replayed.sequence, Some(durable_sequence));
    assert!(matches!(
        &replayed.envelope,
        ComputeEnvelope::Result { result } if result.binding == binding && result.phase == lost
    ));
    requester_again.acknowledge(&replayed).await.unwrap();
    assert!(matches!(
        requester_again.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready {
            workspace_id: 88,
            pending: 1
        }
    ));
    requester_again.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        tilo_test_env::within("a message on requester_again", requester_again.receive())
            .await
            .unwrap()
            .envelope,
        ComputeEnvelope::Pong
    ));
    drop(requester_again);

    let mut after_ack = ComputeClient::connect(
        client_endpoint().await,
        relay_address.clone(),
        user.user_pk,
        Some(relay_key),
        requester.cert,
        requester.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    after_ack.attach(88).await.unwrap();
    assert!(matches!(
        tilo_test_env::within("a message on after_ack", after_ack.receive())
            .await
            .unwrap()
            .envelope,
        ComputeEnvelope::Ready {
            workspace_id: 88,
            pending: 0
        }
    ));

    // The returning worker is handed neither the ended job nor its cancel.
    let mut returning = ComputeClient::connect(
        client_endpoint().await,
        relay_address,
        user.user_pk,
        Some(relay_key),
        worker_cert,
        worker_keypair,
        RevocationList::new(),
    )
    .await
    .unwrap();
    returning.attach(88).await.unwrap();
    assert!(matches!(
        tilo_test_env::within("a message on returning", returning.receive())
            .await
            .unwrap()
            .envelope,
        ComputeEnvelope::Ready {
            workspace_id: 88,
            pending: 0
        }
    ));
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// `device`'s compute session with the relay at `address`, whose device is
/// `relay`.
async fn compute_client(
    address: iroh_base::EndpointAddr,
    user: &TestUser,
    relay: [u8; 32],
    device: &TestDevice,
) -> ComputeClient {
    ComputeClient::connect(
        client_endpoint().await,
        address,
        user.user_pk,
        Some(relay),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap()
}

/// Attach `client` to `workspace_id` and take what the relay replays
/// before it is ready: each delivery's queue sequence and result, in order,
/// and the count `Ready` reports.
async fn replayed_results(
    client: &mut ComputeClient,
    workspace_id: u64,
) -> (Vec<(u64, ComputeResultUpdate)>, u32) {
    client.attach(workspace_id).await.unwrap();
    let mut results = Vec::new();
    loop {
        let delivery = tilo_test_env::within("a replayed delivery", client.receive())
            .await
            .unwrap();
        match delivery.envelope {
            ComputeEnvelope::Ready { pending, .. } => return (results, pending),
            ComputeEnvelope::Result { result } => {
                results.push((delivery.sequence.expect("a durable delivery"), result));
            }
            other => panic!("only results are replayed: {other:?}"),
        }
    }
}

/// Answer `client`'s ping, so every message it sent before was handled.
async fn fence(client: &mut ComputeClient) {
    client.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        tilo_test_env::within("the fence", client.receive())
            .await
            .unwrap()
            .envelope,
        ComputeEnvelope::Pong
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one crash and two restarts, read top to bottom"
)]
/// A relay that goes down with jobs unfinished ends each of them as
/// `relay_restarted` when it comes back, before anyone attaches: the
/// requester is told, durably, until it acknowledges, so it stops waiting
/// and runs the work itself. Coming back again tells it nothing twice, and
/// the returning worker is handed no job that already ended.
async fn compute_session_ends_the_jobs_a_relay_restart_left_unfinished() {
    const WORKSPACE: u64 = 99;
    let user = TestUser::fresh("compute restart owner");
    let relay_device = user.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let requester = user.enrol("requester", DeviceCapabilities::primary());
    let requester_key = requester.device_pubkey();
    let worker = user.enrol("worker", DeviceCapabilities::host());
    let worker_key = worker.device_pubkey();
    let members = [requester_key, worker_key];
    let relay = Box::pin(spawn_relay(&user, relay_device, &[WORKSPACE], &members)).await;

    let mut worker_client =
        compute_client(relay.relay.node_addr(), &user, relay_key, &worker).await;
    assert_eq!(
        replayed_results(&mut worker_client, WORKSPACE).await,
        (Vec::new(), 0)
    );
    let issued_at_ms = unix_now_ms();
    worker_client
        .send(&ComputeEnvelope::Advertise {
            workspace_id: WORKSPACE,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker: worker_key,
                workspace_id: WORKSPACE,
                apps: BTreeSet::from(["org.tilo.render".into()]),
                accelerators: BTreeSet::new(),
                assignment_capacity: 2,
                active_assignments: 0,
                environment_capacity: 0,
                active_environments: 0,
                power: PowerAvailability::External,
                energy_saver: false,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(30_000),
            },
        })
        .await
        .unwrap();
    let mut requester_client =
        compute_client(relay.relay.node_addr(), &user, relay_key, &requester).await;
    assert_eq!(
        replayed_results(&mut requester_client, WORKSPACE).await,
        (Vec::new(), 0)
    );
    let job = |job_id: u8| ComputeJobSpec {
        version: 1,
        job_id: [job_id; 16],
        requester: requester_key,
        worker: worker_key,
        workspace_id: WORKSPACE,
        app_id: "org.tilo.render".into(),
        package_digest: [27; 32],
        entrypoint: "render".into(),
        arguments: Vec::new(),
        inputs: Vec::new(),
        limits: ComputeResourceLimits {
            cpu_time_ms: 5_000,
            memory_bytes: 32 * 1024 * 1024,
            output_bytes: 1024,
            gpu: false,
        },
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(30_000),
    };

    // The worker takes the first job and reports it running; the requester
    // hears so and acknowledges.
    let running = job(28);
    requester_client.submit(&running).await.unwrap();
    let delivery = tilo_test_env::within("the first request", worker_client.receive())
        .await
        .unwrap();
    assert!(matches!(
        &delivery.envelope,
        ComputeEnvelope::Request { spec } if spec == &running
    ));
    worker_client.acknowledge(&delivery).await.unwrap();
    worker_client
        .send(&ComputeEnvelope::Status {
            update: ComputeStatusUpdate {
                binding: ComputeJobBinding {
                    workspace_id: WORKSPACE,
                    job_id: running.job_id,
                    requester: requester_key,
                    worker: worker_key,
                },
                sequence: 1,
                phase: ComputeJobPhase::Running,
                log_tail: Vec::new(),
            },
        })
        .await
        .unwrap();
    let status = tilo_test_env::within("the running status", requester_client.receive())
        .await
        .unwrap();
    assert!(matches!(
        status.envelope,
        ComputeEnvelope::Status {
            update: ComputeStatusUpdate {
                phase: ComputeJobPhase::Running,
                ..
            }
        }
    ));
    requester_client.acknowledge(&status).await.unwrap();
    // The second job reaches the worker, which never acknowledges it.
    let pending = job(29);
    requester_client.submit(&pending).await.unwrap();
    let delivery = tilo_test_env::within("the second request", worker_client.receive())
        .await
        .unwrap();
    assert!(matches!(
        &delivery.envelope,
        ComputeEnvelope::Request { spec } if spec == &pending
    ));
    fence(&mut worker_client).await;
    fence(&mut requester_client).await;

    // The relay crashes here: its compute state is what is on disk now.
    let crashed = tempfile::tempdir().unwrap();
    let workspace_dir =
        |root: &std::path::Path| root.join("workspaces").join(WORKSPACE.to_string());
    for state in ["compute", "compute-pending"] {
        copy_tree(
            &workspace_dir(&relay.state_root).join(state),
            &workspace_dir(crashed.path()).join(state),
        );
    }
    drop((worker_client, requester_client, relay));

    let failed = ComputeJobPhase::Failed {
        code: "relay_restarted".into(),
    };
    let restarted_device = user.enrol("relay restarted", DeviceCapabilities::host());
    let restarted_key = restarted_device.device_pubkey();
    let restarted = Box::pin(spawn_relay_on(
        crashed.path(),
        &user,
        restarted_device,
        &[WORKSPACE],
        &members,
    ))
    .await;
    let mut requester_again =
        compute_client(restarted.node_addr(), &user, restarted_key, &requester).await;
    let (told, ready) = replayed_results(&mut requester_again, WORKSPACE).await;
    assert_eq!(ready, 2);
    assert_eq!(
        told.iter()
            .map(|(_, result)| (result.binding.job_id, result.sequence, result.phase.clone()))
            .collect::<Vec<_>>(),
        vec![([28; 16], 2, failed.clone()), ([29; 16], 1, failed)],
        "each unfinished job ends once, after what its requester already heard"
    );
    drop(requester_again);
    restarted.endpoint.close().await;
    drop(restarted);

    // Down and up again, unacknowledged: the same two failures, no more.
    let again_device = user.enrol("relay restarted again", DeviceCapabilities::host());
    let again_key = again_device.device_pubkey();
    let again = Box::pin(spawn_relay_on(
        crashed.path(),
        &user,
        again_device,
        &[WORKSPACE],
        &members,
    ))
    .await;
    let mut requester_once_more =
        compute_client(again.node_addr(), &user, again_key, &requester).await;
    let (replayed, ready) = replayed_results(&mut requester_once_more, WORKSPACE).await;
    assert_eq!(ready, 2);
    assert_eq!(replayed, told, "the same deliveries, not new ones");
    for (sequence, _) in &replayed {
        requester_once_more
            .send(&ComputeEnvelope::Ack {
                workspace_id: WORKSPACE,
                sequence: *sequence,
            })
            .await
            .unwrap();
    }
    fence(&mut requester_once_more).await;
    drop(requester_once_more);

    // Acknowledged, they are gone; the worker is handed nothing.
    let mut acknowledged = compute_client(again.node_addr(), &user, again_key, &requester).await;
    assert_eq!(
        replayed_results(&mut acknowledged, WORKSPACE).await,
        (Vec::new(), 0)
    );
    let mut returning = compute_client(again.node_addr(), &user, again_key, &worker).await;
    assert_eq!(
        replayed_results(&mut returning, WORKSPACE).await,
        (Vec::new(), 0),
        "the returning worker is not handed a job that already ended"
    );
}
