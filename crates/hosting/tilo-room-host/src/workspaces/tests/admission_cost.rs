//! What admitting one more person costs the host, as the workspace grows.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "counts and sizes in a test are small; overflow would be its failure"
)]

use super::*;

/// One person's device entering `workspace` through its door, as a page
/// does: the owner's pre-admission, the challenge, then the proof. `n`
/// tells this arrival apart from every other. Answers how long the proof
/// took the host.
pub(crate) async fn door_arrival(
    workspaces: &Workspaces,
    host: &DeviceCert,
    workspace: &str,
    n: u32,
) -> std::time::Duration {
    use tilo_relay_protocol::workspaces::JoinReply;
    let (_, device_cert) = guest_device();
    let mut id = [0u8; 32];
    id[..4].copy_from_slice(&n.to_le_bytes());
    id[30] = 7;
    let token = id[..8].to_vec();
    let issued = workspaces
        .pre_admit(
            host,
            PreAdmitRequest {
                request_id: id,
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: Some(device_cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    id[31] = 1;
    let proving = std::time::Instant::now();
    let admitted = workspaces
        .admit_proven(
            issued.signed_invitation,
            device_cert,
            id,
            &format!("Member {n}"),
            Some(&token),
        )
        .await;
    let took = proving.elapsed();
    assert!(
        matches!(admitted, JoinReply::Admitted { .. }),
        "{admitted:?}"
    );
    took
}

/// Of one step: how often it ran, its total microseconds, and its total size.
type Step = (u64, u64, u64);

/// The host's own phase timings (`crate::timing`), summed by step.
#[derive(Clone, Default)]
struct Phases(std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Step>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Phases {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        if event.metadata().target() != "tilo_relay::timing" {
            return;
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        let mut words = message.0.split_whitespace();
        if let (Some("TIMING"), Some(step), Some(micros)) =
            (words.next(), words.next(), words.next())
            && let Ok(micros) = micros.parse::<u64>()
        {
            let size = words.next().and_then(|size| size.parse::<u64>().ok());
            let mut phases = self.0.lock().unwrap();
            let entry = phases.entry(step.to_owned()).or_default();
            entry.0 += 1;
            entry.1 += micros;
            entry.2 += size.unwrap_or(0);
        }
    }
}

/// Seed a workspace with `members` arrivals; answers the whole seeding time
/// and the mean proof time of the last ten.
async fn seed(members: u32) -> (std::time::Duration, std::time::Duration) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let phases = Phases::default();
    let _timing = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(phases.clone()).with(
            tracing_subscriber::filter::Targets::new()
                .with_target("tilo_relay::timing", tracing::Level::DEBUG),
        ),
    );
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let seeding = std::time::Instant::now();
    let mut last = Vec::new();
    for n in 0..members {
        if n + 10 == members {
            phases.0.lock().unwrap().clear();
        }
        let took = door_arrival(&workspaces, &cert, &workspace, n).await;
        if n + 10 >= members {
            last.push(took);
        }
    }
    let seeded = seeding.elapsed();
    for (step, (count, micros, size)) in phases.0.lock().unwrap().iter() {
        eprintln!(
            "HOSTPHASE members={members} {step} x{count} mean {}us size {}",
            micros / count.max(&1),
            size / count.max(&1)
        );
    }
    drop(workspaces);
    relay.shutdown().await;
    let mean = last.iter().sum::<std::time::Duration>() / u32::try_from(last.len()).unwrap();
    (seeded, mean)
}

#[tokio::test]
#[ignore = "a measurement: run with --ignored --nocapture"]
async fn admission_cost_at_thirty_and_three_hundred_members() {
    for members in [30, 300] {
        let (seeded, proof) = seed(members).await;
        eprintln!("ADMISSION members={members} seeding={seeded:?} proof_last10={proof:?}");
    }
}

/// What the host reads of the workspace's governance for each of the last
/// `arrivals` of `members` people coming through the door: member rows
/// decoded, signatures checked, whole reads of the governance and replays
/// of its membership log.
async fn work_per_arrival(
    members: u32,
    arrivals: u32,
) -> Vec<tilo_peer_state::governance_work::GovernanceWork> {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let mut work = Vec::new();
    for n in 0..members + arrivals {
        let _ = tilo_peer_state::governance_work::take();
        door_arrival(&workspaces, &cert, &workspace, n).await;
        if n >= members {
            work.push(tilo_peer_state::governance_work::take());
        }
    }
    drop(workspaces);
    relay.shutdown().await;
    work
}

/// Letting one more person in costs the host the same in a workspace of 40
/// as in one of 10: it decodes no member's row, checks no member's
/// signature, and neither reads the governance whole nor replays the
/// membership history for the people already there. Counted rather than timed, so a slow machine neither hides nor
/// fakes a cost that grows with the workspace. Of a few arrivals the middle
/// one counts: now and then an arrival starts the workspace's publication
/// over, which is paid once per many arrivals, not by each.
#[tokio::test]
async fn letting_one_more_person_in_costs_the_same_however_many_are_in() {
    let middle = |mut work: Vec<tilo_peer_state::governance_work::GovernanceWork>| {
        work.sort_by_key(|work| {
            (
                work.identities_decoded,
                work.signatures_verified,
                work.governance_reads,
                work.replays,
            )
        });
        work[work.len() / 2]
    };
    let small = middle(work_per_arrival(10, 5).await);
    let large = middle(work_per_arrival(40, 5).await);
    assert_eq!(
        small, large,
        "an arrival among 40 people costs what one among 10 does"
    );
    // Each state an admission writes is read from the one before it, with
    // what the admission wrote read again, and the membership is the one
    // before it with the newcomer's receipt folded in: neither walks the
    // workspace's rows nor replays its membership history.
    assert_eq!(
        (large.governance_reads, large.replays),
        (0, 0),
        "an arrival reads the governance whole or replays the membership: {large:?}"
    );
}

/// The size of a workspace's record as written for the last of `members`
/// people coming through the door, without the peer state's parts it holds
/// inline, and of the files beside it.
async fn record_after(members: u32) -> (usize, u64) {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    for n in 0..members {
        door_arrival(&workspaces, &cert, &workspace, n).await;
    }
    let path = workspaces
        .workspace(&workspace)
        .await
        .unwrap()
        .lock()
        .await
        .path
        .clone();
    let mut record: super::super::record_format::StoredRecord =
        postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    record.peer.clear();
    let own = postcard::to_stdvec(&record).unwrap().len();
    let size = |path: &std::path::Path| std::fs::metadata(path).map_or(0, |file| file.len());
    let beside = size(&path.with_extension(super::super::PUBLICATIONS))
        + size(&path.with_extension(super::super::RECEIPTS));
    drop(workspaces);
    relay.shutdown().await;
    (own, beside)
}

/// The record a workspace writes with every change holds no more of its own
/// for 40 people than for 10: their join receipts and the publications since
/// the workspace's base are appended to the files beside it, and the record
/// names how many those hold. (The peer state's parts it names are the peer
/// state's own layout.)
#[tokio::test]
async fn the_record_written_for_an_arrival_does_not_grow_with_the_workspace() {
    let (small, small_beside) = record_after(10).await;
    let (large, large_beside) = record_after(40).await;
    assert!(
        large <= small + 8,
        "the record of 40 people holds {large} B of its own, of 10 people {small} B"
    );
    assert!(
        large_beside > small_beside,
        "what the 30 more people added is kept beside the record"
    );
}
