//! What admitting one more person costs the host, as the workspace grows.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "counts and sizes in a test are small; overflow would be its failure"
)]

use super::*;

/// One person's device entering `workspace` through its entry, as a page
/// does: the owner's pre-admission, then the device's proof with the
/// admission it signs itself. `n` tells this arrival apart from every
/// other. Answers what the host did for it: the device's own signing, which
/// reads the member list it signs against on the device, is the device's.
pub(crate) async fn entry_arrival(
    workspaces: &Workspaces,
    host: &DeviceCert,
    workspace: &str,
    n: u32,
) -> tilo_peer_state::work::Work {
    use tilo_server_protocol::workspaces::JoinReply;
    let (device_key, device_cert) = guest_device();
    let mut id = [0u8; 32];
    id[..4].copy_from_slice(&n.to_le_bytes());
    id[30] = 7;
    let token = id[..8].to_vec();
    let name = format!("Member {n}");
    let before = tilo_peer_state::work::now();
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
    let signing = tilo_peer_state::work::now();
    let admission = workspaces
        .own_admission_of(
            &issued.signed_invitation,
            (&device_cert, &device_key),
            id,
            &name,
        )
        .await
        .unwrap();
    let signed = tilo_peer_state::work::now();
    let admitted = workspaces
        .redeem_on_connection(
            super::super::pages::Presented {
                invitation: issued.signed_invitation,
                cert: device_cert,
                request_id: id,
            },
            admission,
            &name,
            Some(&token),
        )
        .await;
    let host_work = tilo_peer_state::work::now()
        .since(before)
        .since(signed.since(signing));
    assert!(
        matches!(admitted, JoinReply::Admitted { .. }),
        "{admitted:?}"
    );
    host_work
}

/// What the host reads of the workspace's governance for each of the last
/// `arrivals` of `members` people coming through the entry: member rows
/// decoded, signatures checked, whole reads of the governance and replays
/// of its membership log.
async fn work_per_arrival(members: u32, arrivals: u32) -> Vec<tilo_peer_state::work::Work> {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let workspace = created.workspace;
    let mut work = Vec::new();
    for n in 0..members + arrivals {
        let host_work = entry_arrival(&workspaces, &cert, &workspace, n).await;
        if n >= members {
            work.push(host_work);
        }
    }
    drop(workspaces);
    server.shutdown().await;
    work
}

/// Letting one more person in costs the host the same in a workspace of 40
/// as in one of 10: it decodes no member's row, checks no member's
/// signature or invitation receipt, and neither reads the governance whole
/// nor replays the membership history for the people already there.
/// Counted rather than timed, so a slow machine neither hides
/// nor fakes a cost that grows with the workspace. Of a few arrivals the
/// middle one counts: now and then an arrival starts the workspace's
/// publication over, which is paid once per many
/// arrivals, not by each.
#[tokio::test]
async fn letting_one_more_person_in_costs_the_same_however_many_are_in() {
    let middle = |mut work: Vec<tilo_peer_state::work::Work>| {
        work.sort_by_key(|work| {
            (
                work.identities_decoded,
                work.signatures_verified,
                work.admission_reads,
                work.replays,
                work.receipts_read,
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
        (large.admission_reads, large.replays),
        (0, 0),
        "an arrival reads the governance whole or replays the membership: {large:?}"
    );
}

/// The size of a workspace's record as written for the last of `members`
/// people coming through the entry, without the peer state's parts it holds
/// inline, and of the files beside it.
async fn document_after(members: u32) -> (usize, u64) {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    for n in 0..members {
        entry_arrival(&workspaces, &cert, &workspace, n).await;
    }
    let path = workspaces
        .workspace(&workspace)
        .await
        .unwrap()
        .lock()
        .await
        .path
        .clone();
    let mut record: super::super::document_format::StoredResource =
        postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    record.peer.clear();
    let own = postcard::to_stdvec(&record).unwrap().len();
    let size = |path: &std::path::Path| std::fs::metadata(path).map_or(0, |file| file.len());
    let beside = size(&path.with_extension(super::super::PUBLICATIONS))
        + size(&path.with_extension(super::super::RECEIPTS));
    drop(workspaces);
    server.shutdown().await;
    (own, beside)
}

/// The record a workspace writes with every change holds no more of its own
/// for 40 people than for 10: their join receipts and the publications since
/// the workspace's base are appended to the files beside it, and the record
/// names how many those hold. (The peer state's parts it names are the peer
/// state's own layout.)
#[tokio::test]
async fn the_record_written_for_an_arrival_does_not_grow_with_the_workspace() {
    let (small, small_beside) = document_after(10).await;
    let (large, large_beside) = document_after(40).await;
    assert!(
        large <= small + 8,
        "the record of 40 people holds {large} B of its own, of 10 people {small} B"
    );
    assert!(
        large_beside > small_beside,
        "what the 30 more people added is kept beside the record"
    );
}
