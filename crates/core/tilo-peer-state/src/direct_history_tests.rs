//! A device joining a workspace takes in what its host holds at the same cost
//! per record however much the workspace holds: a newcomer to a tracker of
//! eighty parts waits for eighty parts, not for something that grows faster
//! than they do or never arrives at all.

use super::{DIRECT_ALPN, DirectSync, Replica};
use crate::loopback::{addr, endpoint};
use crate::owner::testing::{APP, WORKSPACE, finalize, governed_fixture_at, member_key};
use crate::owner::{NativeWorkspaceOwner, open_file};
use crate::store::{Appetite, Store};
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use std::time::Duration;
use tilo_desktop_model::WorkspaceId;

/// The bytes of one part's text: eighty of them are more than a link takes
/// in one frame, as the parts of a tracker of 16,000 issues are.
const PART_TEXT: usize = 1536 * 1024;

/// The changes each part takes after it started: an item each.
const CHANGES_EACH: usize = 6;

const ALPHABET: &[u8; 64] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .";

/// Text that does not compress: what a part's titles and descriptions are to
/// a deflate pass.
fn text(seed: usize) -> String {
    let mut state = u64::try_from(seed)
        .unwrap()
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        | 1;
    (0..PART_TEXT)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(ALPHABET[usize::try_from(state % 64).unwrap()])
        })
        .collect()
}

/// The bytes this process read and wrote so far, as the kernel counts them:
/// its stores' files and its links' sockets alike. Each test runs in a
/// process of its own.
fn bytes_moved() -> u64 {
    let counted = std::fs::read_to_string("/proc/self/io").unwrap();
    let field = |name: &str| {
        counted
            .lines()
            .find_map(|line| line.strip_prefix(name)?.trim().parse::<u64>().ok())
            .unwrap()
    };
    field("rchar:").saturating_add(field("wchar:"))
}

/// A device that joined a workspace whose host keeps `parts` records of a part's
/// text and [`CHANGES_EACH`] changes each, dialling that host: the bytes host
/// and newcomer read and wrote while it took in what the host holds, how many
/// commits its owner made doing so, and how many of the parts it then held
/// whole.
fn join(parts: usize) -> (u64, u64, usize) {
    let ((intent, bootstrap, _, _, now), mut governor, key) =
        governed_fixture_at(tilo_clock::wall());
    let directory = tempfile::tempdir().unwrap();
    let owner = open_file(&directory.path().join("joined-workspaces.json")).unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    owner
        .install(&finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    // The parts as the host wrote them: each started with its text, then
    // changed an item at a time, every change kept at the host's store.
    let mut written = Vec::new();
    let person = governor.owner_certificates()[0].user_public_key;
    let keys = crate::sealed_records::members_keys(WORKSPACE);
    for part in 0..parts {
        let record = format!("part:{part}");
        let artifact = collab_types::document_artifact_id(APP, WORKSPACE, &record);
        let mut seal = |plaintext: &[u8]| {
            keys.seal_content(
                artifact,
                collab_types::ContentRetention::of_frame(plaintext),
                plaintext,
            )
            .map_err(|error| error.to_string())
        };
        let mut document = AutoCommit::new();
        document.put(ROOT, "text", text(part)).unwrap();
        document.commit_edit("issues", "text", &person);
        written.push(
            governor
                .publication_mut()
                .publish_sealed_checkpoint(APP, &record, document.save(), &mut seal, &key, now)
                .unwrap(),
        );
        for change in 0..CHANGES_EACH {
            let before = document.get_heads();
            document
                .put(ROOT, format!("issue {change}"), "a title")
                .unwrap();
            document.commit_edit("issues", "items", &person);
            let changes = document.save_after(&before);
            written.push(
                governor
                    .publication_mut()
                    .publish_sealed_incremental(APP, &record, changes, &mut seal, &key, now)
                    .unwrap(),
            );
        }
    }
    let store = Arc::new(Store::open(directory.path().join("store"), Appetite::StoreAll).unwrap());
    let host = DirectSync::over(Replica::new(governor), key.clone()).with_store(store);
    for frame in &written {
        host.keep(frame, false).unwrap();
    }
    drop(written);
    let generation = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(workspace, |view| Ok(view.generation()))
            .unwrap()
    };
    let before = generation(&owner);
    let member = DirectSync::over(owner.replica(workspace).unwrap(), member_key())
        .owning(owner.clone(), workspace)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let moved = runtime.block_on(async {
        let host_endpoint = endpoint(&key).await;
        let router = iroh::protocol::Router::builder(host_endpoint.clone())
            .accept(DIRECT_ALPN, host.clone())
            .spawn();
        let member_endpoint = endpoint(&member_key()).await;
        let mut caught_up = member.caught_up();
        let start = bytes_moved();
        let mut linking = tokio::spawn({
            let member = member.clone();
            let member_endpoint = member_endpoint.clone();
            let host_addr = addr(&host_endpoint);
            async move { member.connect(&member_endpoint, host_addr).await }
        });
        let took_in = format!("a newcomer to {parts} parts to take the host's records in");
        tilo_test_env::within(took_in, async {
            tokio::select! {
                up = caught_up.wait_for(|up| *up) => drop(up.unwrap()),
                ended = &mut linking => panic!("the link to {parts} parts ended first: {ended:?}"),
            }
        })
        .await;
        let moved = bytes_moved().saturating_sub(start);
        linking.abort();
        tilo_test_env::within("the host to stop serving", router.shutdown())
            .await
            .unwrap();
        tilo_test_env::within("the newcomer to close", member_endpoint.close()).await;
        moved
    });
    let held = (0..parts)
        .filter(|part| {
            member.read(|peer| {
                crate::sealed_records::opened(peer, &keys, (APP, &format!("part:{part}")))
                    .and_then(|bytes| AutoCommit::load(&bytes).ok())
                    .is_some_and(|document| document.length(ROOT) == CHANGES_EACH + 1)
            })
        })
        .count();
    (moved, generation(&owner).saturating_sub(before), held)
}

#[tokio::test]
async fn a_link_does_not_report_a_frame_it_could_not_keep() {
    let now = tilo_clock::wall();
    let (_, mut governor, key) = governed_fixture_at(now);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let store = Arc::new(Store::open(&path, Appetite::StoreAll).unwrap());
    let link = DirectSync::over(Replica::new(governor.clone()), key.clone()).with_store(store);
    let mut heard = link.0.heard.subscribe();
    let mut document = AutoCommit::new();
    document.put(ROOT, "text", "first").unwrap();
    document.commit_edit(
        "issues",
        "text",
        &governor.owner_certificates()[0].user_public_key,
    );
    let keys = key_group::KeyGroup::create(
        WorkspaceId::parse(WORKSPACE).unwrap(),
        key_group::MemberKey::generate("link-test").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, "part:0");
    let message = governor
        .publication_mut()
        .publish_sealed_checkpoint(
            APP,
            "part:0",
            document.save(),
            &mut |plaintext| {
                keys.seal_content(
                    artifact,
                    collab_types::ContentRetention::Checkpoint,
                    plaintext,
                )
                .map_err(|error| error.to_string())
            },
            &key,
            now,
        )
        .unwrap();
    let author = key.verifying_key().to_bytes();
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::write(&path, []).unwrap();

    assert_eq!(
        link.admit_kept(author, message.clone(), false, false).await,
        (true, None)
    );
    assert!(heard.try_recv().is_err());

    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        link.admit_kept(author, message, false, false).await,
        (false, Some(super::Kept::Durable))
    );
    assert!(heard.try_recv().unwrap().report.is_ok());
}

/// What a newcomer moves per part is what it moves per part of a workspace of
/// two: a cost that grows with the workspace outside what the newcomer takes
/// shows as bytes read or written again. Counted rather than timed, so a busy
/// machine neither hides nor fakes it; a link's system calls are not counted,
/// as its timers make them for as long as a join lasts.
#[test]
fn a_newcomer_to_eighty_parts_takes_each_in_as_fast_as_a_newcomer_to_two() {
    let (small, small_commits, small_held) = join(2);
    let (large, large_commits, large_held) = join(80);
    eprintln!(
        "2 parts: {small} bytes, {small_commits} commits; 80 parts: {large} bytes, \
         {large_commits} commits"
    );
    assert_eq!(small_held, 2, "the newcomer holds both parts");
    assert_eq!(large_held, 80, "the newcomer holds all eighty parts");
    assert!(
        large_commits < 40,
        "taking in 80 parts committed the device's workspace {large_commits} times"
    );
    // Per part, at most a quarter more: large / 80 <= small / 2 * 5 / 4.
    assert!(
        large.saturating_mul(2).saturating_mul(4) <= small.saturating_mul(80).saturating_mul(5),
        "a part of 80 moved {} bytes, a part of 2 {}",
        large / 80,
        small / 2
    );
}

/// `key`'s channel governance for the workspace's group `group`: its genesis,
/// its commit at `epoch`, or its access request.
fn channel_frame(
    key: &SigningKey,
    group: collab_types::WorkspaceId,
    kind: u8,
) -> crate::NeighbourMsg {
    use collab_types::{MlsAccessRequestV1, MlsGovernanceAction, MlsGovernanceFrame};
    match kind {
        0 => crate::NeighbourMsg::MlsGovernance {
            workspace: WORKSPACE.into(),
            frame: Box::new(
                MlsGovernanceFrame::signed(group, [0; 32], 0, MlsGovernanceAction::Genesis, key)
                    .unwrap(),
            ),
            delivery: 0,
        },
        2 => crate::NeighbourMsg::MlsAccessRequest {
            workspace: WORKSPACE.into(),
            request: Box::new(MlsAccessRequestV1::signed(group, vec![1], 1, key).unwrap()),
            delivery: 0,
        },
        epoch => crate::NeighbourMsg::MlsGovernance {
            workspace: WORKSPACE.into(),
            frame: Box::new(
                MlsGovernanceFrame::signed(
                    group,
                    [epoch; 32],
                    u64::from(epoch),
                    MlsGovernanceAction::Commit {
                        removed: Vec::new(),
                        added: Vec::new(),
                        commit: vec![epoch],
                        joining: None,
                        history: None,
                    },
                    key,
                )
                .unwrap(),
            ),
            delivery: 0,
        },
    }
}

/// A device that links the workspace's host after a restart is handed every
/// channel's genesis, its newest commit and each device's newest access
/// request, read from the host's store alone: what it joins each channel's
/// key group from. A commit the newest replaced is not handed.
#[tokio::test(flavor = "multi_thread")]
async fn a_newcomer_joins_each_channel_from_what_its_host_kept_at_its_store() {
    let ((intent, bootstrap, _, _, now), governor, key) = governed_fixture_at(tilo_clock::wall());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let group = collab_types::WorkspaceId::new();
    let founder = SigningKey::from_bytes(&[41; 32]);
    let genesis = channel_frame(&founder, group, 0);
    let first = channel_frame(&founder, group, 1);
    let newest = channel_frame(&founder, group, 3);
    let request = channel_frame(&key, group, 2);
    {
        let store = Arc::new(Store::open(&path, Appetite::StoreAll).unwrap());
        let host = DirectSync::over(Replica::new(governor.clone()), key.clone()).with_store(store);
        let serving = super::Serving::default();
        serving.serve(host.clone()).unwrap();
        let founder_device = founder.verifying_key().to_bytes();
        // What reached the host over its link: the channel's genesis, then
        // the commits after it, then a member's access request.
        assert!(host.carry(founder_device, &genesis).unwrap());
        assert!(host.carry(founder_device, &first).unwrap());
        assert!(host.carry(founder_device, &newest).unwrap());
        host.send(request.clone()).unwrap();
    }

    // The host starts again over its store, with nothing else of the
    // channels' history: no server's retained copy is read.
    let store = Arc::new(Store::open(&path, Appetite::StoreAll).unwrap());
    let host = DirectSync::over(Replica::new(governor), key.clone()).with_store(store);
    let serving = super::Serving::default();
    serving.serve(host.clone()).unwrap();
    let owner = open_file(&directory.path().join("joined-workspaces.json")).unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    owner
        .install(&finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    let newcomer = DirectSync::over(owner.replica(workspace).unwrap(), member_key())
        .owning(owner.clone(), workspace)
        .unwrap();
    let mut heard = newcomer.heard();
    let host_endpoint = endpoint(&key).await;
    let router = iroh::protocol::Router::builder(host_endpoint.clone())
        .accept(DIRECT_ALPN, host.clone())
        .spawn();
    let newcomer_endpoint = endpoint(&member_key()).await;
    let linking = tokio::spawn({
        let newcomer = newcomer.clone();
        let newcomer_endpoint = newcomer_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { newcomer.connect(&newcomer_endpoint, host_addr).await }
    });
    let wanted = [genesis, newest, request];
    let mut handed = Vec::new();
    let handed_all = tokio::time::timeout(Duration::from_secs(60), async {
        while !wanted.iter().all(|frame| handed.contains(frame)) {
            let Ok(frame) = heard.recv().await else {
                break;
            };
            if frame.message.governance_workspace().is_some() {
                handed.push(frame.message);
            }
        }
    })
    .await;
    linking.abort();
    router.shutdown().await.unwrap();
    newcomer_endpoint.close().await;
    assert!(
        handed_all.is_ok(),
        "the newcomer was handed only {handed:?}"
    );
    assert_eq!(
        handed.iter().position(|frame| *frame == wanted[0]),
        Some(0),
        "the group's genesis comes before what builds on it"
    );
    assert!(
        !handed.contains(&first),
        "the commit the newest replaced is not handed"
    );
}
