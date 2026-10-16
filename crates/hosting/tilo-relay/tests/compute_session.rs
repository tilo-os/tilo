//! Dedicated personal-compute ALPN integration coverage.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use std::collections::BTreeSet;

use tilo_clock::unix_now_ms;
use tilo_relay::{
    ComputeClient, ComputeEnvelope, ComputeJobBinding, ComputeJobPhase, ComputeJobSpec,
    ComputeResourceLimits, ComputeStatusUpdate, ComputeWorkerAdvertisement, PowerAvailability,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

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
